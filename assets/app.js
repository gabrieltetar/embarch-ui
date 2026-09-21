// embarch-ui client-side glue: tab switching, theme toggle, SSE plumbing.
// Zero-build (decision 2) — plain vanilla JS, no
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
  // caller (`embarch-topology` decision 19): a topology mismatch
  // relayed by embarch-api points a human at `#topology` rather than at
  // whichever tab that browser happened to have open last. Unknown or absent
  // fragment -> null, and the stored/default tab wins as before.
  // Everything up to the first `?`: no fragment carries parameters of its
  // own any more (the Trace tab's `#trace?study=…&tap=…` went with that tab),
  // and splitting anyway costs nothing and keeps an old bookmark landing on a
  // tab rather than nowhere.
  // A fragment naming a tab that has been folded into another one resolves
  // to its new home rather than to null: `#enroll` was a real address —
  // typed, bookmarked, and the destination an agent was told to point a
  // human at — and the Enroll tab's whole surface is on `#topology` now.
  // Falling through to the stored tab instead would land that link on
  // whichever tab that browser happened to have open last.
  const RETIRED_FRAGMENTS = { enroll: "topology" };

  function tabFromHash() {
    const raw = (location.hash || "").replace(/^#/, "").split("?")[0];
    const name = RETIRED_FRAGMENTS[raw] || raw;
    return document.querySelector(`.nav-item[data-tab="${CSS.escape(name)}"]`) ? name : null;
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

  // The suite's current hardware-topology scope (`embarch-topology`
  // decision 10): one DUT + one dev-bench per machine — a fixed pair of
  // roles, not a dynamically-discovered list.
  const ROLES = [
    { role: "dev-bench", label: "Dev bench" },
    { role: "dut", label: "DUT" },
  ];

  // **What Validate topology last said about each role**, keyed by role —
  // the only thing that puts a status on a box (decision 45). Empty until a
  // pass runs, and deliberately not refreshed by the five-second snapshot
  // poll: a badge that moved on its own would be claiming a live check that
  // nobody ran. Cleared when a role's bindings change underneath it, so a
  // pass can never describe a board that is no longer there.
  let roleStatus = {};

  function statusColor(status) {
    if (status === "pass") return "var(--success)";
    if (status === "fail") return "var(--danger)";
    if (status === "warn") return "var(--warning)";
    return "var(--text-tertiary)";
  }

  function statusText(entry) {
    if (entry.status === "pass") return "● validated " + entry.at;
    if (entry.status === "fail") return "● failed — see the report";
    if (entry.status === "warn") return "● " + entry.short;
    return "● " + entry.short;
  }

  // The label a role is *shown* as. The wire spelling stays lowercase
  // everywhere it is sent (`dut`, `dev-bench`); this is the only place the
  // two differ, and they differ because "dut" rendered in a table reads as
  // a name someone typed rather than as the fixed slot it is.
  function roleLabel(role) {
    const found = ROLES.find((r) => r.role === role);
    return found ? found.label : role;
  }

  function findEnrolled(snapshot, role) {
    return (snapshot.enrolled || []).find((b) => b.role === role) || null;
  }

  // A board is "attached" when one of its declared serials (the JTAG
  // probe's own serial, or — for dev-bench — its separate runtime-link
  // serial, `embarch-topology` decision 17) matches a
  // currently-enumerated probe's serial number.
  function isAttached(snapshot, board) {
    if (!board) return false;
    const probes = snapshot.probes || [];
    // A row with no probe bound is not "attached" — and `null === null`
    // must never make it look like one, which is what a bare equality
    // against two nullable fields would do.
    return probes.some(
      (p) =>
        (!!board.probe_serial && p.serial_number === board.probe_serial) ||
        (!!board.link_port_serial && p.serial_number === board.link_port_serial)
    );
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

  // Role/board/probe/confirmed-timestamp rows — the Dashboard's "Enrolled
  // boards" table, and its only caller since the Topology tab's own roles
  // table folded into the diagram (decision 45). It lists every enrolled
  // entry, whatever its role happens to be named.
  //
  // The last column is labeled "Enrolled" (never "Validated"/"Confirmed"/
  // "Verified"): `confirmed_at_utc_ms` is enrolment time, unmoving until
  // someone re-enrolls, so a header implying a live check would answer a
  // question this field cannot answer. `embarch-core` decision 57
  // (decisions/enrollment.md) declined to persist a real last-validation
  // instant next to it; showing real freshness needs `POST /validate` and
  // that response's own `validated_at_utc_ms`, a live, hardware-touching
  // call this snapshot poll never makes.
  function enrolledTableRows(snapshot) {
    const enrolled = snapshot.enrolled || [];
    if (enrolled.length === 0) {
      return '<tr><td colspan="4" class="placeholder-note">none enrolled yet</td></tr>';
    }
    return enrolled.map((b) => (
      "<tr><td>" + escapeHtml(roleLabel(b.role)) + '</td><td class="mono" title="' +
      escapeHtml(b.name || "") + '">' +
      escapeHtml(boardTypeLabel(b.role, b.name) || b.chip || "—") +
      // A role can hold a board type and no probe (decision 45); both
      // cells say so rather than rendering an empty string that reads as a
      // value.
      '</td><td class="mono">' + escapeHtml(b.probe_serial || "no probe") + "</td><td>" +
      (b.confirmed_at_utc_ms ? formatTimestamp(b.confirmed_at_utc_ms) : "never") + "</td></tr>"
    )).join("");
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

    // **The role is the box's title, and what is in it is stacked under
    // that** (decisions 44, 45): the board *type*, which is clickable and
    // opens a picker, and then a status line that is blank until Validate
    // topology has run. The chip is gone from the picture — it is a
    // property of the board type, shown where board types are listed, and
    // on the diagram it was one more thing to read past.
    function boxBoard(board) {
      if (!board || !board.name) return "pick a board type";
      return boardTypeLabel(board.role, board.name);
    }

    // **The box a role is drawn as is the target a probe is dropped on**
    // (decision 43). The whole node — rect and both labels — is one `<g>`
    // so a drop on the chip name counts as a drop on the box; a `<rect>`
    // alone would leave the text a dead zone sitting on top of it. The
    // role travels in the markup rather than in a closure because this
    // whole string is rebuilt on every snapshot, so no listener bound to
    // an element here would survive the next poll — the listeners live on
    // the `<svg>` and read this attribute (`initEnrollOnDiagram`).
    function roleNode(role, x, centre, label, boardText, hasBoard) {
      // The board line is its own `<g>` with its own hit area, sitting on
      // top of the drop target: clicking it picks a board type, dragging a
      // probe anywhere on the box binds a probe. Two bindings, two
      // gestures, one box — which is the whole model (decision 45).
      const boardFill = hasBoard ? "var(--accent)" : "var(--text-tertiary)";
      const status = roleStatus[role];
      const statusLine = status
        ? '<text x="' + centre + '" y="150" text-anchor="middle" fill="' + statusColor(status.status) +
          '" font-size="11" font-weight="600" font-family="IBM Plex Sans, sans-serif">' +
          escapeHtml(statusText(status)) + "</text>"
        : "";
      return '<g class="topo-drop" data-enroll-role="' + role + '">' +
        '<rect x="' + x + '" y="76" width="200" height="88" rx="12" fill="var(--bg-surface-2)" stroke="var(--border-strong)" stroke-width="1.4"/>' +
        '<text x="' + centre + '" y="104" text-anchor="middle" fill="var(--text-primary)" font-size="14" font-weight="600" font-family="IBM Plex Sans, sans-serif">' + escapeHtml(label) + "</text>" +
        '<g class="topo-board-pick" data-board-role="' + role + '">' +
        '<rect x="' + (x + 12) + '" y="112" width="176" height="20" rx="6" fill="transparent"/>' +
        '<text x="' + centre + '" y="127" text-anchor="middle" fill="' + boardFill +
        '" font-size="11.5" font-family="IBM Plex Mono, monospace" style="cursor:pointer; text-decoration:underline; text-underline-offset:3px;">' +
        escapeHtml(boardText) + "</text></g>" +
        statusLine +
        "</g>";
    }

    svg.innerHTML =
      '<rect x="30" y="80" width="220" height="80" rx="12" fill="var(--bg-surface-2)" stroke="var(--border-strong)" stroke-width="1.4"/>' +
      '<text x="140" y="112" text-anchor="middle" fill="var(--text-primary)" font-size="14" font-weight="600" font-family="IBM Plex Sans, sans-serif">this machine</text>' +
      '<text x="140" y="132" text-anchor="middle" fill="var(--text-tertiary)" font-size="11.5" font-family="IBM Plex Mono, monospace">' +
      (snapshot.core_reachable ? "embarch-core" : "embarch-core (unreachable)") + "</text>" +

      roleNode("dev-bench", 390, 490, roleLabel("dev-bench"), boxBoard(devBench), !!(devBench && devBench.name)) +
      roleNode("dut", 740, 840, roleLabel("dut"), boxBoard(dut), !!(dut && dut.name)) +

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

    // A snapshot landing mid-drag rebuilds the node under the cursor, and
    // with it the class that says "you are over this box". The drag is
    // still in progress, so the highlight is restored rather than left for
    // the next `dragover` — which, over a node the pointer is already
    // inside and no longer moving across, may not come.
    if (dragoverRole) {
      const g = svg.querySelector('[data-enroll-role="' + CSS.escape(dragoverRole) + '"]');
      if (g) g.classList.add("dragover");
    }
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
          '<button class="btn" data-signal-edit="' + escapeHtml(sig.name) + '">Move route</button> ' +
          '<button class="btn btn-icon-danger" data-signal-remove="' + escapeHtml(sig.name) +
          '" title="Retract this signal">&#10005;</button>' +
          "</td></tr>"
      )
      .join("");
  }

  function renderTopology(snapshot) {
    renderErrorBanner("topology-error", snapshot);
    renderTopologyDiagram(snapshot);
    renderProbePool(snapshot);
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
    // beside it (`embarch-topology` decision 18's amendment —
    // Alert's shape is board-specific and a wire has none of those fields).
    // `carrierCell` is where that shows, per row.
  }

  // --- Enrolling, on the Topology tab --------------------------------------
  // Drag-and-drop, inherited from embarch-core's own retired `/enroll` page
  // (embarch-core/src/enroll_page.rs) by way of this UI's own retired Enroll
  // tab — but submitting through `/api/enroll`, which already holds a live
  // `CoreClient` server-side, rather than asking a human to paste in Core's
  // bearer token by hand the way that static page had to.
  //
  // **The drop target is the diagram, not a pair of rectangles beside it**
  // (decision 43): the same box that says a role is empty is the box a probe
  // is dropped onto to fill it.
  let selectedSerial = null;
  let lastProbesKey = null;
  let latestSnapshot = null;
  let dragoverRole = null;

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

  // **The dialog does not invent a board type, and it does not ask for a
  // chip** (decision 45). Binding a probe attaches to real silicon, and
  // what to attach as is a property of the board type in the role — so the
  // list is that role's served picker, defaulted to what the role already
  // holds, and the chip field is a read-out of the chosen entry. A role
  // with no board type yet is named as such, and the enrol is refused
  // rather than guessed.
  function fillAssignBoards(role, current) {
    const select = document.getElementById("assign-board");
    if (!select) return;
    const options = rolePickers[role] || [];
    if (!options.length) {
      select.innerHTML = '<option value="">no board type available for this role</option>';
    } else {
      select.innerHTML = options
        .map(function (o) {
          return '<option value="' + escapeHtml(o.board) + '" data-chip="' + escapeHtml(o.chip) +
            '">' + escapeHtml(o.label) + "</option>";
        })
        .join("");
    }
    if (current) select.value = current;
    syncAssignChip();
  }

  function syncAssignChip() {
    const select = document.getElementById("assign-board");
    const chip = document.getElementById("assign-chip");
    if (!select || !chip) return;
    chip.value = select.selectedOptions.length
      ? select.selectedOptions[0].getAttribute("data-chip") || ""
      : "";
  }

  // **Re-enrolling is the migration path, not a mistake** — the same shape
  // as re-declaring a signal to move its route (decision 10), so an occupied
  // role opens the ordinary dialog rather than refusing the drop. Two
  // consequences are deliberate: the chip field arrives pre-filled from the
  // enrolment being replaced, because a board that moved to another probe
  // is the same chip and retyping it is the only way to get it wrong; and
  // the dialog names what it displaces, so a mis-drop is visible before the
  // button is pressed rather than after the row is gone.
  function openAssignDialog(serial, role) {
    const existing = findEnrolled(latestSnapshot || {}, role);
    document.getElementById("assign-probe-label").textContent = probeLabel(serial);
    document.getElementById("assign-role-label").textContent = roleLabel(role);
    fillAssignBoards(role, existing && existing.name ? existing.name : "");
    const note = document.getElementById("assign-replace-note");
    if (existing && existing.probe_serial) {
      note.style.display = "block";
      note.textContent = "replaces the probe already bound to this role: " + existing.probe_serial;
    } else {
      note.style.display = "none";
      note.textContent = "";
    }
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
    const name = document.getElementById("assign-board").value;
    if (!name || !chip) {
      result.innerHTML =
        '<span style="color:var(--danger);">pick a board type for this role first — binding a ' +
        "probe has to attach as some chip, and that is the board type's</span>";
      return;
    }
    result.textContent = "enrolling…";
    try {
      const resp = await fetch("/api/enroll", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ role, chip, probe_serial: serial, name }),
      });
      const text = await resp.text();
      if (!resp.ok) {
        result.innerHTML = '<span style="color:var(--danger);">' + resp.status + " " + escapeHtml(text) + "</span>";
        return;
      }
      const board = JSON.parse(text);
      result.innerHTML =
        '<span style="color:var(--success);">enrolled ' + escapeHtml(roleLabel(board.role)) +
        " — " + escapeHtml(board.name || "unnamed") + ", chip " + escapeHtml(board.chip) + "</span>";
      selectedSerial = null;
      // A new probe binding is a new thing to check; whatever the last
      // Validate said about this role described the old one.
      delete roleStatus[role];
      setTimeout(closeAssignDialog, 700);
    } catch (e) {
      result.innerHTML = '<span style="color:var(--danger);">' + escapeHtml(String(e)) + "</span>";
    }
  }

  // Listeners live on the `<svg>`, never on a node: `renderTopologyDiagram`
  // rewrites the whole picture on every snapshot, so anything bound to a
  // role box is gone within five seconds of being bound. Delegation also
  // means a box that does not exist yet — the diagram before the first
  // snapshot lands — becomes droppable the moment it is drawn.
  function initEnrollOnDiagram() {
    const svg = document.getElementById("topology-diagram");
    if (svg) {
      const roleAt = (ev) => {
        const g = ev.target && ev.target.closest && ev.target.closest("[data-enroll-role]");
        return g ? g.getAttribute("data-enroll-role") : null;
      };
      const clearHighlight = () => {
        svg.querySelectorAll(".topo-drop.dragover").forEach((g) => g.classList.remove("dragover"));
        dragoverRole = null;
      };
      svg.addEventListener("dragover", (ev) => {
        const role = roleAt(ev);
        if (!role) return;
        // Only a node accepts the drop. Without the `preventDefault` the
        // browser treats the whole SVG as a non-target and no `drop` ever
        // fires; calling it unconditionally would make the empty space
        // between the boxes look droppable and then swallow the drop.
        ev.preventDefault();
        if (role !== dragoverRole) {
          clearHighlight();
          dragoverRole = role;
          const g = ev.target.closest("[data-enroll-role]");
          if (g) g.classList.add("dragover");
        }
      });
      svg.addEventListener("dragleave", (ev) => {
        if (roleAt(ev) === dragoverRole) clearHighlight();
      });
      svg.addEventListener("drop", (ev) => {
        const role = roleAt(ev);
        clearHighlight();
        if (!role) return;
        ev.preventDefault();
        const serial = ev.dataTransfer.getData("text/plain");
        if (serial) openAssignDialog(serial, role);
      });
      // Click-to-assign fallback for anyone who'd rather select-then-click
      // than drag — same flow, different trigger; also what makes this
      // usable on touch devices, where native drag-and-drop is patchy.
      svg.addEventListener("click", (ev) => {
        const role = roleAt(ev);
        if (role && selectedSerial) openAssignDialog(selectedSerial, role);
      });
    }
    const cancelBtn = document.getElementById("assign-cancel");
    const confirmBtn = document.getElementById("assign-confirm");
    const backdrop = document.getElementById("assign-dialog-backdrop");
    const boardSelect = document.getElementById("assign-board");
    if (boardSelect) boardSelect.addEventListener("change", syncAssignChip);
    if (cancelBtn) cancelBtn.addEventListener("click", closeAssignDialog);
    if (confirmBtn) confirmBtn.addEventListener("click", confirmAssign);
    if (backdrop) backdrop.addEventListener("click", closeAssignDialog);
  }

  function renderSnapshot(snapshot) {
    latestSnapshot = snapshot;
    renderStatusChip(snapshot);
    renderDashboard(snapshot);
    renderTopology(snapshot);
  }

  // Suite-wide SSE convergence (decision 6): one
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

  // --- Debug tab (decision 7) ---------------------------------------------
  // Backlog via one `/recent` fetch on load, then live lines over a `/events`
  // SSE stream (never re-fetching `/recent` on a timer — decision 6).
  //
  // **That SSE stream is embarch-ui's own, served to this browser** — it is
  // not Core's. Decision 7's amendment retired *Core's* `GET /logs/stream`;
  // embarch-ui reaches Core by polling `/logs/recent` server-side
  // (`src/logs.rs::poll_loop`, decision 26) and pushes the diff out over
  // `/api/logs/events`. Two different things are called SSE in this tab, and
  // reading the line above as "Core streams to us" has now misled two
  // readers — hence this paragraph.
  //
  // Two sources, one viewer (decision 13). embarch-core's lines
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
    // **Stored, not tailed**, which is why it carries no `events`: these are
    // finished builds, and a finished build's log does not change. The
    // picker beside the chips is what selects one — the source is a
    // directory, not a stream, and the other two sources have nothing to
    // pick.
    //
    // It is here rather than in a Builds tab of its own because the Debug
    // tab is already the place that switches between log sources
    // (decision 13), and a build log is a log.
    builds: {
      stored: true,
      subtitle: "Firmware builds this UI ran — the whole log of each, kept on this machine",
      errorTitle: "build log unreadable",
      empty: "no firmware build has been run from this UI yet",
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
  // reason, `embarch-api` decision 43.)
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

  // Bumped on every source switch. A backlog fetch is asynchronous and the
  // console it lands in is whichever one is mounted when it *resolves*, not
  // the one that was mounted when it was issued — so the fetch carries the
  // generation it was issued under and drops its answer if that has moved on.
  //
  // **The ordering in `attachLogSource` is not what prevents this**, though a
  // comment there used to claim it was: closing the stream and clearing the
  // console first does nothing about a `/recent` request already in flight.
  // Switching Core -> embarch-api while Core's backlog was still arriving
  // spliced 200 of Core's lines into embarch-api's console, under
  // embarch-api's heading, with no timestamp order between them.
  let logGeneration = 0;

  // Which stored build log the `builds` source is showing. Kept across a
  // source switch so going core -> builds -> core -> builds comes back to
  // the same one rather than jumping to the newest.
  let logBuildId = null;

  // Fills the picker beside the chips. Newest first, which is the one
  // somebody switching to this source almost always wants, so it is also the
  // default selection.
  async function loadBuildLogList() {
    const pick = document.getElementById("log-build-pick");
    if (!pick) return;
    let logs = [];
    try {
      const resp = await fetch("/api/build/logs");
      logs = (await resp.json()).logs || [];
    } catch (e) {
      logs = [];
    }
    pick.innerHTML = "";
    if (!logs.length) {
      const opt = document.createElement("option");
      opt.value = "";
      opt.textContent = "no builds yet";
      pick.appendChild(opt);
      logBuildId = null;
      return;
    }
    logs.forEach((log) => {
      const opt = document.createElement("option");
      opt.value = log.id;
      const when = new Date(log.started_utc_ms).toLocaleString();
      opt.textContent = when + " · " + log.project + " · " + (log.ok ? "ok" : "failed");
      pick.appendChild(opt);
    });
    if (!logs.some((l) => l.id === logBuildId)) logBuildId = logs[0].id;
    pick.value = logBuildId;
  }

  async function loadStoredBuildLog() {
    const generation = logGeneration;
    if (!logBuildId) {
      renderLogsError(null);
      return;
    }
    try {
      const resp = await fetch("/api/build/logs/" + encodeURIComponent(logBuildId));
      const text = await resp.text();
      if (generation !== logGeneration) return;
      if (!resp.ok) {
        renderLogsError(text);
        return;
      }
      renderLogsError(null);
      appendLogLines(text.split("\n"));
    } catch (e) {
      if (generation !== logGeneration) return;
      renderLogsError(String(e));
    }
  }

  async function loadLogBacklog() {
    const generation = logGeneration;
    if (LOG_SOURCES[logSource].stored) {
      await loadBuildLogList();
      if (generation !== logGeneration) return;
      await loadStoredBuildLog();
      return;
    }
    try {
      const resp = await fetch(LOG_SOURCES[logSource].recent);
      const text = await resp.text();
      if (generation !== logGeneration) return;
      if (!resp.ok) {
        renderLogsError(text);
        return;
      }
      renderLogsError(null);
      const data = JSON.parse(text);
      appendLogLines(data.lines || []);
    } catch (e) {
      if (generation !== logGeneration) return;
      renderLogsError(String(e));
    }
  }

  // Tears down whatever was streaming, resets the console to the new
  // source's placeholder, then reloads backlog and reopens the stream —
  // invalidating any backlog fetch still in flight for the old source on the
  // way past (`logGeneration`).
  function attachLogSource() {
    logGeneration += 1;
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

    // The picker only belongs to the stored source; the other two have
    // nothing to pick.
    const pick = document.getElementById("log-build-pick");
    if (pick) pick.style.display = config.stored ? "" : "none";

    loadLogBacklog();
    // A stored log is a finished file. There is no stream to open, and
    // opening one against an absent `events` would be a 404 per switch.
    if (config.stored) return;
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

    const pick = document.getElementById("log-build-pick");
    if (pick) {
      pick.addEventListener("change", () => {
        logBuildId = pick.value || null;
        // Same generation bump a source switch does, for the same reason: a
        // fetch already in flight for the previous build must not splice its
        // lines into this one's console.
        logGeneration += 1;
        const console_ = document.getElementById("log-console");
        if (console_) console_.innerHTML = "";
        loadStoredBuildLog();
      });
    }

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


  // --- a study's steps and its record, rendered once for every reader ------
  //
  // These live here, above both the Study Designer and the Live Study tab,
  // because **two copies of them is the exact defect decisions 20 and 23
  // exist to stop.** `decodeOutcome` is the one place a step outcome is read
  // (23), `sdRunningStepLabel` the one place Core's `current_step` becomes a
  // human number (20), and `recordsCell` the one place a capture's record
  // check becomes words. A second rendering of any of them would be a second
  // chance for a failed step, a short capture or an unchecked one to read as
  // its opposite.

  /// One decoder for a step outcome, whichever of the two wire shapes it
  /// arrived in (embarch-ui decision 23):
  ///   - **tagged**, from `events.json`, `GET /study/{id}`'s `result`, and the
  ///     SSE `StepCompleted`: `"Pass"` | `{"Fail":{"reason":…}}` | `"TimedOut"`.
  ///   - **flattened**, from `GET /study/{id}/steps`: a bare `"Pass"` /
  ///     `"Fail"` / `"TimedOut"` string with `reason` as a sibling field.
  /// `reason` is that sibling field; it is ignored when `outcome` is the
  /// tagged `Fail` shape, which carries its own.
  ///
  /// Anything that is not one of those three variants — `undefined`, `null`,
  /// a number, an object without `.Fail`, a typo'd string — comes back
  /// `kind: "unknown"` rather than being folded into "pass" or "neutral".
  /// The two call sites below both render "unknown" as visibly wrong: a
  /// step that failed must never read as a step that did not, and neither
  /// may a step this code simply failed to parse.
  function decodeOutcome(outcome, reason) {
    if (outcome === "Pass") return { kind: "pass", reason: null };
    if (outcome === "TimedOut") return { kind: "timedout", reason: null };
    if (outcome === "Fail") return { kind: "fail", reason: reason || null };
    if (outcome && typeof outcome === "object" && outcome.Fail) {
      return { kind: "fail", reason: (outcome.Fail && outcome.Fail.reason) || null };
    }
    return { kind: "unknown", reason: null };
  }

  function outcomeBadge(outcome, reason) {
    var d = decodeOutcome(outcome, reason);
    if (d.kind === "pass") return '<span class="badge badge-success">Pass</span>';
    if (d.kind === "timedout") return '<span class="badge badge-warning">TimedOut</span>';
    if (d.kind === "fail") {
      return '<span class="badge badge-danger">Fail</span> <span class="mono" style="font-size:11.5px;">' + escapeHtml(d.reason || "no reason given") + "</span>";
    }
    // Unknown shape: a red "?" badge, never the neutral dash this used to
    // fall through to — see decision 23.
    return '<span class="badge badge-danger">?</span> <span class="mono" style="font-size:11.5px;">unrecognised outcome</span>';
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
    // (`embarch-study-designer` decision 44). Shown on *every*
    // step, not only a security one, and that is the point: the same
    // failure at L1 and at L4 are different findings, and this column is
    // the only place a reader can tell them apart. Rendered verbatim from
    // the server's own value — this file never maps a level to a claim
    // about it.
    if (step.security_level) {
      parts.push(String(step.security_level).toUpperCase());
    }
    var text = parts.length ? escapeHtml(parts.join(" · ")) : "";

    /* What a `RunProtocol` step's machine ended as (`embarch-study-designer`
     * decision 62), through the same `outcomeBadge` every other outcome goes
     * through — one decoder, so a protocol that failed cannot read as one
     * that did not.
     *
     * `final_state` is rendered **verbatim and with no claim about it**.
     * Whether that state was terminal is a lookup in the ProtocolDef the
     * study carries, which this file does not have; asserting "finished" or
     * "stopped here" from the name alone would be the kind of plausible,
     * wrong reading this tab keeps refusing to produce. */
    if (step.protocol) {
      var badge = outcomeBadge(step.protocol.outcome, null);
      var state =
        '<span class="mono" style="font-size:11.5px;">ended in ' +
        escapeHtml(step.protocol.final_state || "—") + "</span>";
      text = (text ? text + " · " : "") + badge + " " + state;
    }
    return text || '<span class="placeholder-note">—</span>';
  }

  // Decision 11: a result renders **how** each version was established, not
  // just what it was. `verified` is decided server-side by
  // `VersionSource::is_verified` — re-deriving it here is the easiest place to
  // accidentally reintroduce the exact defect `embarch-study-designer` decision 40 exists to close, so
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

  function renderProvenance(el, prov) {
    if (!el) return;
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

  // A run's taps as the study's own record reports them: how many bytes each
  // wrote, whether the capture is short of what its source produced, and what
  // its records verified to.
  //
  // **No "open in trace" button any more, and its absence is the point.** The
  // chart is a card further down this same page now, on the same study — the
  // button existed to carry a study_id from one tab to another, and there is
  // no longer another tab to carry it to.
  function renderRunStreams(el, studyId, streams) {
    if (!el) return;
    if (!streams || !streams.length) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.innerHTML =
      '<div class="card-title" style="margin-bottom:8px;">Captured streams</div>' +
      '<table class="data-table"><thead><tr><th>Tap</th><th>Bytes</th><th>Complete</th>' +
      "<th>Records</th><th></th>" +
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
            "<td>" + recordsCell(ref.records) + "</td>" +
            '<td style="text-align:right;"><a class="btn" download href="/api/studies/' +
            encodeURIComponent(studyId || "") + "/stream/" + encodeURIComponent(ref.name) +
            '/download">Download</a></td></tr>'
          );
        })
        .join("") +
      "</tbody></table>";
  }

  /* What checking one capture's records found — a **reading**, not a badge
   * (decision 12): the interesting answers here are differences, and a
   * green/red pill collapses "4 of 5 verified, one at offset 2048" into a
   * colour.
   *
   * Three invariants, each of which has a wrong version that looks right:
   *
   *   1. `records: null` is NEVER rendered as clean. The study declared no
   *      framing for this tap, which is a different fact from "every record
   *      verified" — conflating them is how a short capture read as complete
   *      in the first place. It reads "not checked".
   *
   *   2. `total === 0` is NEVER rendered as verified. `RecordReport`'s own
   *      `all_verified()` returns true for an empty capture, which is why
   *      this deliberately does not use it: a tap that captured nothing has
   *      nothing to verify, and saying so is the honest answer.
   *
   *   3. The 32-offset report cap is NEVER written here. It is inferred, by
   *      comparing how many offsets arrived with how many records failed —
   *      a literal 32 in this file is a second copy of
   *      `MAX_BAD_RECORDS_REPORTED` that goes stale the day it moves. */
  function recordsCell(report) {
    if (report == null) {
      return '<span class="placeholder-note">not checked — this tap declared no record framing</span>';
    }
    var total = report.total || 0;
    var verified = report.verified || 0;
    var bad = total - verified;
    var offsets = report.bad_offsets || [];
    var leading = report.leading_bytes || 0;

    if (total === 0) {
      return (
        '<span class="placeholder-note">nothing to check — no record found in this capture' +
        (leading ? " (" + leading + " leading bytes)" : "") +
        "</span>"
      );
    }

    var head =
      '<span class="mono">' + verified + " of " + total + " verified</span>";
    var detail = [];
    if (leading) {
      detail.push(
        leading +
          " byte" + (leading === 1 ? "" : "s") +
          " before the first record — the capture began mid-record"
      );
    }
    if (bad > 0) {
      /* Inferred, never read off a constant: fewer offsets than failures
       * means the report hit its own cap. */
      var capped = offsets.length < bad;
      detail.push(
        bad + " did not verify at " +
        (capped ? "the first " + offsets.length + " of " + bad + " offsets " : "offsets ") +
        offsets.join(", ")
      );
    }
    return (
      head +
      (detail.length
        ? '<div class="placeholder-note" style="margin-top:4px;">' +
          escapeHtml(detail.join(" · ")) + "</div>"
        : "")
    );
  }

  // The run badge's counter names **the step now running**, not the count of
  // steps finished (decisions/study-designer.md decision 20). During a run
  // this badge is the *only* thing on the card that says where the study is —
  // the step rows are not filled in until it completes — so it answers "which
  // step am I waiting on".
  //
  // Core's `current_step` is the 0-based index of the last step that
  // *finished*, and is absent until one has (`embarch-core/interfaces.md`,
  // `GET /study/{id}`; embarch-core decision 43). So the 1-based step now in
  // flight is `current_step + 2`, and `1` while it is still null — not the
  // `+ 1` this used to do, which was written for a count convention Core does
  // not send and read one step short at every moment a step was in flight.
  //
  // The clamp is load-bearing: after the last step lands there is a window,
  // up to one poll long, in which Core still reports `running`, and `3/2`
  // would be nonsense. A zero-step study (`total_steps: 0`) has no step to
  // name, so it gets no counter rather than `1/0`.
  function sdRunningStepLabel(currentStep, totalSteps) {
    if (totalSteps == null || totalSteps < 1) return "";
    var step = currentStep == null ? 1 : currentStep + 2;
    if (step > totalSteps) step = totalSteps;
    return " " + step + "/" + totalSteps;
  }

  // --- Study Designer tab --------------------------------------------------
  //
  // Authoring happens server-side in `embarch-study-designer` (the merged
  // action list, the registry, table-rows -> `Study`); this file's job is
  // only to collect what the engineer typed and hand it over unaltered.
  // The one thing it does interpret is byte input — and only mechanically,
  // text -> UTF-8 or hex tokens -> bytes, never a number encoded into a
  // width/endianness nobody here is in a position to know
  // (`embarch-study-designer` decision 35).

  var sdRows = [];
  var sdActions = [];        // MergedAction[] from GET /api/study-designer/actions
  var sdRegistry = [];       // RegisteredAction[] — the subset with fields to pick
  // Notify/indicate-capable characteristics, from the same response — what a
  // selective monitor row and a GATT tap both pick from (`embarch-study-designer` decisions 53/55).
  var sdSubscribable = [];
  /* Group headings for the target picker (decision 17), keyed by hyphenated
   * *service* UUID — `embarch-study-designer` decision 56's
   * 2026-08-26 amendment. Same fallback rule as `charLabel`: an unnamed
   * service is its UUID head, never an invented name. */
  var sdServiceNames = {};
  /* `limits::MAX_MONITOR_TARGETS`, served rather than restated here so the
   * browser and `build_study` cannot disagree about the cap. The fallback
   * only applies to a response written before the field existed. */
  var sdMaxTargets = 16;
  /* `limits::MAX_STREAM_NAME_LEN`, served on the actions response beside
   * `max_monitor_targets` (task ui/003). No numeric fallback on purpose,
   * unlike `sdMaxTargets` above: a wrong guess here would silently re-commit
   * the exact restated-limit defect this field was added to remove. `null`
   * means "not yet known, or a server old enough not to send it," and every
   * reader below treats it as "don't guess the cap" rather than "assume 32." */
  var sdMaxStreamNameLen = null;
  /* The picker's working copy while its dialog is open (decision 17).
   * Cancel discards it; Done commits it to the row. Held outside the row so
   * a half-made selection never reaches `sdCollectRows`. */
  var sdTargetDraft = null;
  /* The firmware repo's own study-structs.toml entries (`embarch-study-designer`
   * decision 52), each `{name, header:[{name,type}], repeat:[...]}`.
   *
   * Objects rather than the bare names this used to hold: the layout editor
   * has to render what a layout IS before it can edit it, and the tap's
   * decoder dropdown still reads only `.name`. One served shape read by
   * both, rather than a second route over the same file. */
  var sdStructLayouts = [];
  /* Everything else the server serves that the browser must not restate.
   *
   * **Every one of these follows `sdMaxStreamNameLen`'s posture, not
   * `sdMaxTargets`'s: no numeric fallback, `null`/`[]` meaning "not yet
   * known".** A guessed cap is the restated-limit defect these fields exist
   * to remove, and a guessed vocabulary is worse — it renders a picker whose
   * entries the server would refuse. An empty list is an empty picker, which
   * is a refusal an author can see. */
  var sdProtocols = [];
  /* Stems of `.eap` files that did not parse. A row naming a protocol that
   * is in none of the served summaries reads differently when this is
   * non-empty: "not in this repo" is a claim, and with an unreadable file in
   * the directory it is one this browser cannot make. */
  var sdUnparsedEap = [];
  var sdMaxProtocols = null;
  var sdMaxRecordMagicLen = null;
  var sdMaxStructFields = null;
  var sdScalarTypes = [];
  var sdLogLevels = [];
  /* `{max_steps_per_study, max_event_arms_per_state, max_protocols_wire_len}`
   * — the advisory dev-bench caps. `null` renders as **unknown**, never as
   * "within caps": a bench this tab could not read is not a bench that
   * agrees. Nothing here disables Run. */
  var sdDevBenchLimits = null;
  /* The study's own dev-bench log level (`embarch-dev-bench` decision 39).
   * `null` means "not stated", which leaves the crate's own default in
   * place rather than this browser sending a level it invented. */
  var sdLogLevel = null;
  /* The name the Study name box was last *filled* with — by a load, by New
   * study, or by the markup's own initial value. It is how "the box still
   * holds the open study's name" is told apart from "somebody typed a name
   * here", which is the whole of what New study needs to know; see
   * `sdNewStudy`. */
  var sdLoadedName = "untitled-study";
  /* The saved-study list keyed by slug, so picking one can tell an editable
   * study from a run-only file **before** fetching it — which is what keeps
   * the browser from calling a route it already knows will answer 409. */
  var sdStudyIndex = {};
  /* Which run the version-check dialog is standing in front of.
   *
   * `{kind: "authored"}` is the table; `{kind: "stored", slug}` is a file on
   * disk. One dialog either way, because the question it asks — does the
   * bench match what this study requires — is the same question, and a
   * second dialog would be a second place to get that answer wrong. */
  var sdPendingRun = { kind: "authored" };
  /* The run-only study currently previewed, or null. */
  var sdStoredSlug = null;
  /* The name a registration dialog is editing, or null for a new one. Sent
   * as `previous_name`, which is what makes edit and rename one form. */
  var sdRegPrevious = null;
  /* The same, for the layout dialog. */
  var sdLayoutPrevious = null;
  /* The `.eap` editor's whole model: the scanned files, which one is open,
   * whether it has unsaved edits, and the errors currently rendered. */
  var sdEapFiles = [];
  var sdEapStem = null;
  var sdEapDirty = false;
  var sdEapErrors = [];
  // Characteristic display names, keyed by hyphenated characteristic UUID
  // (`embarch-study-designer` decision 56). Empty is the honest
  // starting state and every reader falls back to the UUID.
  var sdCharNames = {};
  var sdNextRowId = 1;

  /* The built-in action vocabulary used to be written out here as nine
   * hand-copied {value, label} pairs, while the crate served its own list
   * that this file filtered out and threw away. The two drifted, exactly the
   * way decision 17 says a browser-side copy of a server-side fact drifts:
   * the served list still held seven after `embarch-study-designer` decision 53 added two, and
   * nothing caught it, because a list nobody renders cannot look wrong.
   *
   * Now `sdBuiltIns()` reads the served entries, labels and all
   * (`embarch-study-designer::BuiltInActionKind`, suite/017). Adding a
   * built-in is one edit, in the crate.
   *
   * Empty until the first actions response lands — the picker renders an
   * empty Built-in group for that moment rather than a stale guess, which is
   * the same posture `sdMaxStreamNameLen` takes and for the same reason. */
  function sdBuiltIns() {
    return sdActions
      .filter(function (a) { return a.BuiltIn; })
      .map(function (a) { return { value: a.BuiltIn.which, label: a.BuiltIn.label }; });
  }

  // Which built-ins take a characteristic selection (`embarch-study-designer` decision 53).
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

  /* What a picker's option is labelled with (`embarch-study-designer`
   * decision 56): the vendor's name for the characteristic, or the C
   * identifier the firmware declared it under, or — when nothing named it —
   * the UUID head this showed for everything before `embarch-study-designer` decision 56.
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
   * (`embarch-study-designer` decision 56, amended 2026-08-26). */
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
   * monitor row and a GATT tap both pick from (`embarch-study-designer` decision 53), and the payload
   * layouts a tap can decode with (`embarch-study-designer` decision 52).
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
    if (data && typeof data.max_stream_name_len === "number") {
      sdMaxStreamNameLen = data.max_stream_name_len;
    }
    sdProtocols = (data && data.protocols) || [];
    sdUnparsedEap = (data && data.unparsed_files) || [];
    sdScalarTypes = (data && data.scalar_types) || [];
    sdLogLevels = (data && data.dev_bench_log_levels) || [];
    if (data && typeof data.max_protocols_per_study === "number") {
      sdMaxProtocols = data.max_protocols_per_study;
    }
    if (data && typeof data.max_record_magic_len === "number") {
      sdMaxRecordMagicLen = data.max_record_magic_len;
    }
    if (data && typeof data.max_struct_fields === "number") {
      sdMaxStructFields = data.max_struct_fields;
    }
    sdDevBenchLimits = (data && data.dev_bench_limits) || null;
    renderSdLogLevels();
    renderSdCapsNote();
  }

  /* The dev-bench log level picker, built from the served vocabulary.
   *
   * An empty list leaves the select empty and says so — the browser holds
   * no copy of the levels, so a response that did not carry them is a
   * picker with nothing in it rather than a picker of guesses. */
  function renderSdLogLevels() {
    var select = sdEl("sd-log-level");
    if (!select) return;
    if (!sdLogLevels.length) {
      select.innerHTML = '<option value="">not served</option>';
      select.disabled = true;
      sdEl("sd-log-level-note").textContent =
        "the log levels come from the server and this response carried none";
      return;
    }
    select.disabled = false;
    select.innerHTML = sdLogLevels
      .map(function (level) {
        return (
          '<option value="' + escapeHtml(level.value) + '"' +
          (level.value === sdLogLevel ? " selected" : "") + ">" +
          escapeHtml(level.label) + "</option>"
        );
      })
      .join("");
    if (sdLogLevel == null) {
      /* Nothing stated: show the level the server flagged as the default,
       * without *claiming* it — `sdLogLevelPayload` keeps sending null until
       * an author picks one, so the study still gets the crate's default
       * rather than one this browser asserted.
       *
       * The flag is served rather than matched by name here: which level is
       * the default is a fact of `DevBenchLogLevel`, and selecting nothing
       * would show the first option, which is the one level a study must
       * never reach by not choosing. */
      var fallback = sdLogLevels.filter(function (l) { return l.default; })[0];
      select.value = (fallback || sdLogLevels[0]).value;
    }
    renderSdLogLevelNote();
  }

  /* What choosing the selected level costs, in the server's own words.
   *
   * The prose is served beside the label for the same reason the label is:
   * prose keyed on a level name would be a browser-side copy of the level
   * set. **No invented clamp reading** — the note points at the bench's own
   * log stream because that is where the firmware reports its clamp, and
   * nothing here has read it. */
  function renderSdLogLevelNote() {
    var note = sdEl("sd-log-level-note");
    if (!note) return;
    var value = sdEl("sd-log-level").value;
    var level = sdLogLevels.filter(function (l) { return l.value === value; })[0];
    note.textContent = (level && level.note) || "";
  }

  /* What this study is sending, against what this suite's dev-bench takes.
   *
   * `null` limits render as **unknown**, never as "within caps" — a bench
   * this tab could not read is not a bench that agrees. Within caps renders
   * as nothing, because a quiet note is what "nothing to say" looks like.
   *
   * Only the step cap is computed here. The other two cannot be: a wire
   * length is a postcard encoding and an event-arm count is a property of a
   * resolved ProtocolDef, so both come off `/preflight`. */
  function renderSdCapsNote() {
    var note = sdEl("sd-caps-note");
    if (!note) return;
    if (!sdDevBenchLimits) {
      note.innerHTML =
        "dev-bench capacity <strong>unknown</strong> — nothing here has read it, so nothing " +
        "here can say whether this study is within it.";
      return;
    }
    var max = sdDevBenchLimits.max_steps_per_study;
    if (typeof max !== "number" || sdRows.length <= max) {
      note.textContent = "";
      return;
    }
    note.innerHTML =
      "<strong>" + sdRows.length + " steps</strong> — this suite's dev-bench refuses a study " +
      "over " + max + " at decode. Advisory only: the bench in front of you may be a " +
      "different build, so Run is not blocked.";
  }

  /* The level to send, or `null` for "not stated".
   *
   * Null rather than the default level spelled here: `build_authored`
   * leaves the crate's own argued
   * default in place for an absent level, and a browser that sent Warn
   * explicitly would be a second copy of that decision. */
  function sdLogLevelPayload() {
    return sdLogLevel;
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

  // Vendor-defined services (`embarch-study-designer` decision
  // 41) — Nordic's UART Service and anything else the crate's `vendor` table
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

  /* The `RunProtocol` row's two selects, both from the served protocol list.
   *
   * Three cases are **wrong but visible** — each keeps what was authored,
   * says what is wrong with it, and lets the server refuse it. Snapping a
   * row to a different protocol, or silently blanking a state, is how a
   * study quietly becomes a different study:
   *
   *   1. A protocol the repo no longer declares: kept, marked "not in this
   *      repo".
   *   2. A protocol whose file did not parse: its option is `disabled` and
   *      the state picker reads "unknown until the file parses" — NOT "no
   *      states", which would be a claim about the manifest rather than
   *      about our ability to read it.
   *   3. A terminal entry state: refused before submit, from the served
   *      `terminal` flag. A run entering one would pass instantly and
   *      capture nothing. */
  function sdRunProtocolParamsHtml(row) {
    if (!sdProtocols.length) {
      return (
        '<span class="placeholder-note">no <span class="mono">.eap</span> protocol in this ' +
        "repo — author one with <span class=\"mono\">Author .eap files…</span> below</span>"
      );
    }
    var known = sdProtocols.filter(function (p) { return p.name === row.protocol; })[0];
    var options = sdProtocols
      .map(function (p) {
        return (
          '<option value="' + escapeHtml(p.name) + '"' +
          (p.name === row.protocol ? " selected" : "") +
          // A block that parsed but did not resolve cannot be *chosen*: the
          // server would refuse it, and offering it would be offering a
          // choice that only fails later.
          (p.resolved ? "" : " disabled") + ">" + escapeHtml(p.name) +
          (p.resolved ? " (" + p.states.length + " states)" : " — " + escapeHtml(p.file) +
            ".eap has an error") + "</option>"
        );
      })
      .join("");
    if (!row.protocol) {
      options = '<option value="" selected>pick a protocol…</option>' + options;
    } else if (!known) {
      options =
        '<option value="' + escapeHtml(row.protocol) + '" selected>' +
        escapeHtml(row.protocol) + " — not in this repo</option>" + options;
    }

    var stateOptions;
    var note = "";
    if (known && !known.resolved) {
      /* The protocol is there, in a file, with something wrong inside it.
       * "no states" would be a claim about the manifest where this is a
       * statement about our ability to read it. */
      stateOptions =
        '<option value="' + escapeHtml(row.entryState || "") + '" selected>' +
        escapeHtml(row.entryState || "(first state)") +
        " — unknown until the file parses</option>";
      note =
        known.file + ".eap has an error, so this protocol's states are unknown — open " +
        "Author .eap files… to see what is wrong with it";
    } else if (!known) {
      stateOptions =
        '<option value="' + escapeHtml(row.entryState || "") + '" selected>' +
        escapeHtml(row.entryState || "(first state)") + "</option>";
      note = row.protocol
        ? (sdUnparsedEap.length
            ? "not declared by any .eap file this repo could read — and " +
              sdUnparsedEap.length + " file(s) did not parse, so it may be in one of those"
            : "this protocol is not declared by any .eap file in this repo")
        : "";
    } else {
      stateOptions =
        '<option value=""' + (row.entryState ? "" : " selected") + ">(first state)</option>" +
        known.states
          .map(function (st) {
            return (
              '<option value="' + escapeHtml(st.name) + '"' +
              (st.name === row.entryState ? " selected" : "") + ">" + escapeHtml(st.name) +
              (st.terminal ? " — terminal" : "") + "</option>"
            );
          })
          .join("");
      if (row.entryState && !known.states.some(function (st) { return st.name === row.entryState; })) {
        stateOptions =
          '<option value="' + escapeHtml(row.entryState) + '" selected>' +
          escapeHtml(row.entryState) + " — not a state of " + escapeHtml(known.name) +
          "</option>" + stateOptions;
      }
      var entry = known.states.filter(function (st) { return st.name === row.entryState; })[0];
      if (entry && entry.terminal) {
        note =
          "a terminal state — the run would reach its outcome immediately and capture nothing";
      }
    }
    return (
      '<div class="sd-params">' +
      '<label class="sd-param" style="flex:1 1 200px;"><span>Protocol</span>' +
      '<select data-field="protocol">' + options + "</select></label>" +
      '<label class="sd-param" style="flex:1 1 200px;"><span>Entry state</span>' +
      '<select data-field="entryState">' + stateOptions + "</select></label>" +
      (note ? '<div class="sd-error" style="flex:1 1 100%;">' + escapeHtml(note) + "</div>" : "") +
      "</div>"
    );
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
      // to connect to (`embarch-study-designer` decision 43).
      // Blank means "whichever peripheral advertises first", which on a
      // bench with any other BLE device in range is a coin toss.
      targetName: "",
      // Only meaningful for a `ble_security` row (`embarch-study-designer` decision 44). L4 is the
      // level that decision was written for; L1 *is* offered, because
      // decision 44 makes it the honest way to say "this DUT needs none"
      // rather than leaving the step out and hoping.
      securityLevel: "l4",
      // Only meaningful for a `run_protocol` row (`embarch-study-designer`
      // decision 60). **Names, never indices**: a saved study that carried
      // the index would silently mean a different protocol the day an
      // unrelated row was deleted. Blank protocol is refused by the server,
      // because there is no defensible default; blank entry state is the
      // protocol's first declared state.
      protocol: "",
      entryState: "",
      rawService: "",
      rawChar: "",
      // Vendor-defined selection (`embarch-study-designer` decision 41): ids, never UUIDs — the
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
      // Only meaningful for a selective monitor row (`embarch-study-designer` decision 53): the
      // characteristic UUIDs this step subscribes to. Empty is refused
      // server-side rather than promoted to "everything" — quietly
      // subscribing to the whole table is the flood the action exists to
      // avoid.
      targets: [],
      timeout_ms: 15000,
      continue_on_fail: false,
      // The "when" (`embarch-study-designer` decision 42): how long dev-bench waits before starting
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
  // (`embarch-study-designer` decision 36). Getting that order
  // wrong produces an empty capture and no error, so it's offered as one
  // click rather than left to be rediscovered.
  function sdCaptureTemplate() {
    return [
      // Left blank deliberately rather than prefilled with some DUT's name:
      // which device is under test is the engineer's to say, and the row
      // flags itself as "any device!" until they do (`embarch-study-designer` decision 41).
      sdNewRow({ name: "connect", kind: "built_in", which: "ble_connect", timeout_ms: 20000 }),
      sdNewRow({ name: "open-capture", kind: "built_in", which: "gatt_monitor_start", timeout_ms: 20000 }),
      // Prefilled against the Nordic UART Service rather than as a raw row:
      // its UUIDs are Nordic's, not the engineer's, so there is nothing to
      // type here but the payload (`embarch-study-designer` decision 41). The payload is left empty
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
        // response to the stimulus (`embarch-study-designer` decision 42).
        delay_before_ms: 1000,
      }),
      // The old template put a `gatt_monitor_all` step here to hold the run
      // open while the response arrived. A delay does that without a second
      // action re-subscribing inside an already-open window, which is what
      // decision 42 made possible.
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
    sdBuiltIns().forEach(function (b) {
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
      if (row.which === "run_protocol") {
        return sdRunProtocolParamsHtml(row);
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

      var html = '<div class="sd-params">';
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
      renderSdCapsNote();
      renderSdProtocolsCard();
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
    // Live: the step count is the one advisory cap a browser can check for
    // itself, so it answers as the table is edited rather than only at Run.
    renderSdCapsNote();
    // Derived from the rows, so it follows them.
    renderSdProtocolsCard();
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

    if (field === "protocol") {
      row.protocol = ev.target.value;
      // The entry state belongs to a protocol, so changing the protocol
      // clears it rather than carrying a state name onto a machine that may
      // not have one by that name. "" means the first declared state, which
      // is the one entry point every protocol has.
      row.entryState = "";
      renderSdRows();
      return;
    }
    if (field === "entryState") {
      row.entryState = ev.target.value;
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
          // pairs the server parses (`embarch-study-designer` decision 53). The service comes from the
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
        if (row.which === "run_protocol") {
          action.protocol = row.protocol || null;
          action.entry_state = row.entryState || null;
          if (!row.protocol) {
            throw new Error(label + ": pick one of this repo's .eap protocols");
          }
          // Refused here, from the served flag, rather than discovered as a
          // 400 after a round trip — the choice was made in this row and the
          // refusal belongs beside it.
          var picked = sdProtocols.filter(function (p) { return p.name === row.protocol; })[0];
          var st = picked
            ? picked.states.filter(function (x) { return x.name === row.entryState; })[0]
            : null;
          if (st && st.terminal) {
            throw new Error(
              label + ": '" + row.entryState + "' is a terminal state of " + row.protocol +
              ", so the run would reach its outcome immediately and capture nothing"
            );
          }
        }
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
      pool.innerHTML = '<span class="placeholder-note">nothing detected yet — run <span class="mono">Discover GATT (live)</span> with the dev-bench and DUT connected, or <span class="mono">Run GATT extractor</span> to read them out of the firmware source. Both are under <strong>Static firmware analysis</strong> on the project panel.</span>';
      return;
    }
    pool.innerHTML = "";
    items.forEach(function (item) {
      var chip = document.createElement("div");
      chip.className = "probe-card";
      var sources = [];
      if (item.sources.live) sources.push("live");
      if (item.sources.static_extraction) sources.push("source");
      // Named where anything names it (`embarch-study-designer` decision 56). The UUID moves to the
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

  /* Takes one `/actions` response and repaints everything that reads it.
   *
   * **One function, three callers.** This sequence existed three times — in
   * `loadSdActions`, in the discover handler, and in `sdEnterProject` — and
   * the three had already drifted: `sdEnterProject` repainted four of the
   * six things that read this response, which is why the registered-action
   * and layout pools stayed at their index.html placeholders until some
   * unrelated call happened to run one of the other two. Found by clicking
   * the real page; invisible to every test here.
   *
   * A renderer added below is now added once. */
  function sdAdoptActions(data) {
    sdActions = data.actions || [];
    sdRegistry = sdRegisteredActions();
    sdAdoptDiscovery(data);
    renderSdUnregistered();
    renderSdRegistered();
    renderSdLayouts();
    renderSdProtocolsCard();
    renderSdRows();
    // The tap table picks its characteristics and its decoder layouts out
    // of this same response, so it goes stale on exactly this call and no
    // other. Without this a GATT tap row kept reading "no notify-capable
    // characteristic known — run Discover GATT" *after* a discovery that had
    // just found several, which reads as the discovery having failed.
    renderSdTaps();
  }

  async function loadSdActions() {
    var resp = await fetch("/api/study-designer/actions");
    if (!resp.ok) throw new Error(await resp.text());
    var data = await resp.json();
    sdAdoptActions(data);
    return data;
  }

  async function loadSdStudies() {
    var select = sdEl("sd-load-select");
    if (!select) return;
    var resp = await fetch("/api/study-designer/studies");
    if (!resp.ok) return;
    var studies = await resp.json();
    sdStudyIndex = {};
    var current = select.value;
    select.innerHTML = '<option value="">Load saved study…</option>';
    studies.forEach(function (s) {
      sdStudyIndex[s.slug] = s;
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
      body: JSON.stringify({
        name: name,
        rows: rows,
        requires: sdRequiresPayload(),
        taps: sdTaps,
        dev_bench_log_level: sdLogLevelPayload(),
      }),
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

  function sdCloseStored() {
    var panel = sdEl("sd-stored-panel");
    if (!panel) return;
    panel.style.display = "none";
    panel.innerHTML = "";
    sdStoredSlug = null;
  }

  /* Shows a run-only study read-only.
   *
   * **Touches neither `sdRows` nor `sdTaps`.** The table stays exactly as it
   * was, which is why the panel says so out loud: a read-only preview under
   * an unrelated table is otherwise the most natural thing in the world to
   * misread as "this is what is loaded". */
  async function sdOpenStored(slug) {
    var panel = sdEl("sd-stored-panel");
    if (!panel) return;
    sdStoredSlug = slug;
    panel.style.display = "block";
    panel.innerHTML = '<p class="placeholder-note">reading…</p>';
    var resp = await fetch(
      "/api/study-designer/studies/" + encodeURIComponent(slug) + "/summary"
    );
    var text = await resp.text();
    if (!resp.ok) {
      panel.innerHTML = '<p class="sd-error">' + resp.status + " " + escapeHtml(text) + "</p>";
      return;
    }
    var st = JSON.parse(text);
    var line = function (label, value) {
      return (
        '<div class="sd-stored-row"><span class="sd-stored-label">' + escapeHtml(label) +
        '</span><span class="mono">' + escapeHtml(value) + "</span></div>"
      );
    };
    panel.innerHTML =
      '<div class="card-title" style="margin:0 0 6px;">' + escapeHtml(st.name) +
      " — run-only</div>" +
      '<p class="placeholder-note">This file was not written from this table, so it has no ' +
      "rows to load back. <strong>The step table above is a different study and has not " +
      "changed.</strong> It can still be run exactly as it is on disk.</p>" +
      '<div class="sd-stored-grid">' +
      line("requires dev-bench", st.requires.dev_bench_version) +
      line("requires DUT", st.requires.firmware_version) +
      line("dev-bench log level", st.dev_bench_log_level) +
      line("record checks", String(st.record_checks)) +
      (st.protocols.length ? line("protocols", st.protocols.join(", ")) : "") +
      (st.taps.length ? line("taps", st.taps.join(", ")) : "") +
      "</div>" +
      '<ol class="sd-stored-steps">' +
      (st.steps.length
        ? st.steps.map(function (n) { return "<li>" + escapeHtml(n) + "</li>"; }).join("")
        : '<li class="placeholder-note">no steps</li>') +
      "</ol>" +
      '<div class="sd-row-actions" style="margin-top:12px;">' +
      '<span style="flex:1;"></span>' +
      '<button id="sd-stored-run" class="btn btn-primary">Run this file</button></div>';
  }

  /* Runs the stored file, through the **same** version-check dialog an
   * authored run goes through — the discrepancy it shows is a fact about
   * the bench and the study's `requires`, and both exist here just as much.
   * `sdPendingRun` is the whole of the refactor that makes one dialog serve
   * two runs. */
  function sdRunStored() {
    if (!sdStoredSlug) return;
    sdPendingRun = { kind: "stored", slug: sdStoredSlug };
    return sdOpenRunCheck();
  }

  async function sdLoadStudy(slug) {
    if (!slug) return;
    sdShowBuildError("");
    var resp = await fetch("/api/study-designer/studies/" + encodeURIComponent(slug));
    var text = await resp.text();
    if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
    sdApplyStudy(JSON.parse(text));
  }

  /* The one and only path from a `Study` to what is on screen.
   *
   * It exists because there wasn't one. Loading a study set the name, the
   * requirements, the build spec, the log level, the taps and the rows;
   * `sdNewStudy` set the rows and the taps and nothing else — so starting a
   * new study after opening a saved one left the previous study's
   * requirements, its build selection and its log level sitting in the
   * panel, attached to a file that stated none of them. Every one of those
   * would then have been written back out by the first save.
   *
   * The rule this function keeps is that **every field is assigned on every
   * call**, from the study or from a default. A field restored only when the
   * study happens to carry it is the same bug in waiting: the next author to
   * add a property to `Study` gets the leftover for free.
   */
  function sdApplyStudy(loaded) {
    loaded = loaded || {};
    sdShowBuildError("");
    sdEl("sd-name").value = loaded.name || "untitled-study";
    sdLoadedName = sdEl("sd-name").value;
    // `{}` rather than a skipped call when the file states no requirements:
    // `sdApplyRequires` clears both fields and unticks both "any" boxes, and
    // skipping it is exactly how the previous study's version requirement
    // survived into the new one.
    sdApplyRequires(loaded.requires || {});
    sdApplyBuild(
      (loaded.requires && loaded.requires.build) || null,
      (loaded.requires && loaded.requires.outpost) || null
    );
    // Restored, or left unstated when the file predates the field — never
    // silently reset to Warn, which is the same drop decision 17 records for
    // monitor targets.
    sdLogLevel = loaded.dev_bench_log_level || null;
    renderSdLogLevels();
    sdTaps = (loaded.taps || []).map(function (tap) {
      if (tap.kind !== "gatt_notify") return tap;
      tap.record_magic = tap.record_magic || [];
      tap.magicMode = "hex";
      tap.magicText = magicTextFromBytes(tap.record_magic);
      tap.magicError = "";
      return tap;
    });
    renderSdTaps();
    sdRows = (loaded.rows || []).map(function (r) {
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
        // Both, not one: dropping `entry_state` while restoring `protocol`
        // is exactly the shape of the silent loss decision 17 records for
        // monitor targets.
        base.protocol = a.protocol || "";
        base.entryState = a.entry_state || "";
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
    // Derived from the rows, so it is stale until they land — and stale in
    // exactly the way this function exists to stop.
    renderSdProtocolsCard();
    renderSdBuildOptsSummary();
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
    // Reported beside the button as well as into the step table's error
    // line: this button sits on the project card now, and `sd-build-error`
    // is inside `sd-body`, which is hidden whenever no project is open —
    // so on exactly the path where this fails most obviously, the old
    // surface says nothing at all.
    sdStaticNote("discovering against the DUT…", false);
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
        var why = "discover failed: " + resp.status + " " + (await resp.text());
        sdShowBuildError(why);
        sdStaticNote(why, true);
        return;
      }
      var data = await resp.json();
      sdAdoptActions(data);
      sdStaticNote(
        "live discovery found " + (data.subscribable || []).length +
          " notify-capable characteristic" + ((data.subscribable || []).length === 1 ? "" : "s") +
          " — observed off the board, not read out of the source.",
        false
      );
    } catch (e) {
      sdShowBuildError("discover failed: " + String(e));
      sdStaticNote("discover failed: " + String(e), true);
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

  /* ---- Selective-monitor target picker (decision 17) --------------------
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

  /* The one renderer behind both "register this characteristic" and "edit
   * this action": the form is identical, and `sdRegPrevious` is the whole
   * of the difference — absent it is today's upsert, present it makes the
   * request a rename the server reference-checks first. */
  function syncRegisterMode() {
    var editing = sdRegPrevious != null;
    sdEl("sd-reg-save").textContent = editing ? "Save changes" : "Register";
    sdEl("sd-reg-delete").style.display = editing ? "inline-flex" : "none";
    sdEl("sd-reg-refusal").style.display = "none";
    sdEl("sd-reg-refusal").innerHTML = "";
  }

  /* Opens the same dialog on an action that already exists.
   *
   * The entry point this never had: `ActionRegistry::save` has always
   * validated-then-written, and the UI has only ever had an upsert POST with
   * nowhere to launch an edit from. */
  function openRegisterEdit(name) {
    var action = (sdRegistry || []).filter(function (a) { return a.name === name; })[0];
    if (!action) return;
    sdRegPrevious = name;
    sdEl("sd-reg-name").value = action.name;
    sdEl("sd-reg-service").value = uuidStr(action.service_uuid);
    sdEl("sd-reg-char").value = uuidStr(action.uuid);
    sdEl("sd-reg-op").value = action.operation;
    sdEl("sd-reg-fields").innerHTML = "";
    sdEl("sd-reg-result").style.display = "none";
    syncRegFieldsVisibility();
    (action.fields || []).forEach(function (field) {
      addRegField();
      var nodes = sdEl("sd-reg-fields").querySelectorAll(".sd-reg-field");
      var node = nodes[nodes.length - 1];
      node.querySelector('[data-reg="fname"]').value = field.name;
      node.querySelector('[data-reg="foff"]').value = field.byte_offset;
      node.querySelector('[data-reg="flen"]').value = field.byte_len;
      var values = node.querySelector('[data-reg="values"]');
      values.innerHTML = "";
      (field.values || []).forEach(function (value) {
        values.insertAdjacentHTML("beforeend", regValueHtml());
        var vn = values.lastElementChild;
        vn.querySelector('[data-reg="vlabel"]').value = value.label;
        // Hex, not the text it may have been typed as — the saved form is
        // bytes, and re-rendering them as text would be a guess about an
        // encoding the bytes no longer carry.
        vn.querySelector('[data-reg="vmode"]').value = "hex";
        vn.querySelector('[data-reg="vbytes"]').value = (value.bytes || [])
          .map(function (b) { return "0x" + ("0" + b.toString(16)).slice(-2); })
          .join(" ");
      });
    });
    syncRegisterMode();
    sdEl("sd-register-dialog").style.display = "block";
    sdEl("sd-register-backdrop").style.display = "block";
  }

  function openRegisterDialog(serviceUuid, charUuid, properties) {
    sdRegPrevious = null;
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
    syncRegisterMode();
    sdEl("sd-register-dialog").style.display = "block";
    sdEl("sd-register-backdrop").style.display = "block";
  }

  /* Renders a 409's table of the studies still using something, and
   * **leaves the form exactly as it is**.
   *
   * One renderer for all three refusals (action, layout, protocol file):
   * they carry one body shape, and a refusal a reader has to decode
   * differently per dialog is three chances to render it wrong.
   *
   * A body that does not parse as that shape is rendered verbatim — every
   * other error in this suite is plain text, and a refusal from a layer that
   * never heard of this shape must still be readable. */
  function renderRefusal(boxId, status, text) {
    var box = sdEl(boxId);
    if (!box) return;
    var body = null;
    try {
      body = JSON.parse(text);
    } catch (e) {
      body = null;
    }
    box.style.display = "block";
    if (!body || !body.referenced_by) {
      box.innerHTML = '<p class="sd-error">' + status + " " + escapeHtml(text) + "</p>";
      return;
    }
    var rows = (body.referenced_by || [])
      .map(function (r) {
        return (
          '<tr><td class="mono">' + escapeHtml(r.slug) + "</td><td>" + escapeHtml(r.name) +
          '</td><td class="mono">' + escapeHtml((r.steps || []).join(", ")) + "</td></tr>"
        );
      })
      .join("");
    var unscannable = body.unscannable || [];
    box.innerHTML =
      '<p class="sd-error" style="margin-bottom:8px;">' + escapeHtml(body.error || "refused") +
      "</p>" +
      (rows
        ? '<table class="data-table"><thead><tr><th>File</th><th>Study</th><th>Where</th>' +
          "</tr></thead><tbody>" + rows + "</tbody></table>"
        : "") +
      (unscannable.length
        ? '<p class="sd-error" style="margin-top:8px;">Also, these files could not be read, ' +
          "so this cannot say whether they use it: " +
          escapeHtml(unscannable.join("; ")) + "</p>"
        : "") +
      '<p class="placeholder-note" style="margin-top:8px;">Nothing was changed, and the form ' +
      "above still holds what you entered.</p>";
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

    sdEl("sd-reg-refusal").style.display = "none";
    var resp = await fetch("/api/study-designer/registry", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        previous_name: sdRegPrevious,
        name: name,
        service_uuid: serviceBytes,
        uuid: charBytes,
        operation: operation,
        fields: fields,
      }),
    });
    var text = await resp.text();
    if (resp.status === 409) return renderRefusal("sd-reg-refusal", resp.status, text);
    if (!resp.ok) return regResult(resp.status + " " + text, false);
    regResult(
      sdRegPrevious ? "saved" : "registered — it's now pickable as a step action",
      true
    );
    await loadSdActions();
    setTimeout(closeRegisterDialog, 800);
  }

  async function deleteRegistration() {
    if (sdRegPrevious == null) return;
    sdEl("sd-reg-refusal").style.display = "none";
    var resp = await fetch(
      "/api/study-designer/registry/" + encodeURIComponent(sdRegPrevious),
      { method: "DELETE" }
    );
    var text = await resp.text();
    if (resp.status === 409) return renderRefusal("sd-reg-refusal", resp.status, text);
    if (!resp.ok) return regResult(resp.status + " " + text, false);
    await loadSdActions();
    closeRegisterDialog();
  }

  // --- payload layouts ---------------------------------------------------

  /* One field row, reusing `.sd-reg-value`'s layout so this dialog brings no
   * CSS of its own (decision 17).
   *
   * The type dropdown renders **only served `scalar_types`**. An empty list
   * is an empty picker and a refusal — a guessed eighteen would offer
   * spellings the server might not accept, which is worse than offering
   * none. */
  function layoutFieldHtml(field) {
    var options = sdScalarTypes
      .map(function (t) {
        return (
          '<option value="' + escapeHtml(t) + '"' +
          (field && t === field.type ? " selected" : "") + ">" + escapeHtml(t) + "</option>"
        );
      })
      .join("");
    return (
      '<div class="sd-reg-value">' +
      '<input class="sd-input mono" data-layout="name" placeholder="field name" ' +
      'spellcheck="false" value="' + escapeHtml((field && field.name) || "") + '" />' +
      '<select class="sd-input mono" data-layout="type">' +
      (options || '<option value="">no scalar type served</option>') +
      "</select>" +
      '<button class="sd-icon-btn" data-layout="remove" title="remove this field">✕</button>' +
      "</div>"
    );
  }

  function renderLayoutGroup(id, fields) {
    var box = sdEl(id);
    if (!box) return;
    box.innerHTML = (fields || []).map(layoutFieldHtml).join("");
  }

  function readLayoutGroup(id) {
    var out = [];
    sdEl(id).querySelectorAll(".sd-reg-value").forEach(function (node) {
      var name = node.querySelector('[data-layout="name"]').value.trim();
      var type = node.querySelector('[data-layout="type"]').value;
      if (!name && !type) return;
      out.push({ name: name, type: type });
    });
    return out;
  }

  function openLayoutDialog(name) {
    var layout = name
      ? sdStructLayouts.filter(function (l) { return l.name === name; })[0]
      : null;
    sdLayoutPrevious = layout ? layout.name : null;
    sdEl("sd-layout-title").textContent = layout ? "Edit layout" : "New payload layout";
    sdEl("sd-layout-name").value = layout ? layout.name : "";
    renderLayoutGroup("sd-layout-header", layout ? layout.header : []);
    renderLayoutGroup("sd-layout-repeat", layout ? layout.repeat : []);
    sdEl("sd-layout-result").style.display = "none";
    sdEl("sd-layout-refusal").style.display = "none";
    sdEl("sd-layout-refusal").innerHTML = "";
    sdEl("sd-layout-delete").style.display = layout ? "inline-flex" : "none";
    sdEl("sd-layout-dialog").style.display = "block";
    sdEl("sd-layout-backdrop").style.display = "block";
  }

  function closeLayoutDialog() {
    sdEl("sd-layout-dialog").style.display = "none";
    sdEl("sd-layout-backdrop").style.display = "none";
  }

  function layoutResult(message, ok) {
    var el = sdEl("sd-layout-result");
    el.style.display = "block";
    el.className = ok ? "placeholder-note" : "sd-error";
    el.textContent = message;
  }

  async function submitLayout() {
    var name = sdEl("sd-layout-name").value.trim();
    if (!name) return layoutResult("the layout needs a name", false);
    var header = readLayoutGroup("sd-layout-header");
    var repeat = readLayoutGroup("sd-layout-repeat");
    if (!header.length && !repeat.length) {
      return layoutResult("a layout with no field decodes nothing", false);
    }
    var blank = header.concat(repeat).filter(function (f) { return !f.name || !f.type; });
    if (blank.length) return layoutResult("every field needs a name and a type", false);
    if (sdMaxStructFields != null) {
      var over = header.length > sdMaxStructFields || repeat.length > sdMaxStructFields;
      if (over) {
        return layoutResult(
          "each group takes at most " + sdMaxStructFields + " fields",
          false
        );
      }
    }
    sdEl("sd-layout-refusal").style.display = "none";
    var resp = await fetch("/api/study-designer/structs", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        previous_name: sdLayoutPrevious,
        name: name,
        header: header,
        repeat: repeat,
      }),
    });
    var text = await resp.text();
    if (resp.status === 409) return renderRefusal("sd-layout-refusal", resp.status, text);
    if (!resp.ok) return layoutResult(resp.status + " " + text, false);
    layoutResult("saved", true);
    // Through the actions response, so the tap's Decoding dropdown and this
    // dialog read the same list — proving the widened shape reached both.
    await loadSdActions();
    renderSdTaps();
    setTimeout(closeLayoutDialog, 600);
  }

  async function deleteLayout() {
    if (sdLayoutPrevious == null) return;
    sdEl("sd-layout-refusal").style.display = "none";
    var resp = await fetch(
      "/api/study-designer/structs/" + encodeURIComponent(sdLayoutPrevious),
      { method: "DELETE" }
    );
    var text = await resp.text();
    if (resp.status === 409) return renderRefusal("sd-layout-refusal", resp.status, text);
    if (!resp.ok) return layoutResult(resp.status + " " + text, false);
    await loadSdActions();
    renderSdTaps();
    closeLayoutDialog();
  }

  // --- .eap protocol manifests -------------------------------------------

  /* The read-only Protocols card: which protocols this study's rows name.
   *
   * Derived from the rows, exactly as `build_study` derives what the study
   * carries — a separately-authored list here would be a browser-side copy
   * of something already implied, which is the staleness pattern this whole
   * design keeps refusing. */
  function renderSdProtocolsCard() {
    var box = sdEl("sd-protocols-carried");
    if (!box) return;
    var named = [];
    sdRows.forEach(function (row) {
      if (row.kind !== "built_in" || row.which !== "run_protocol") return;
      if (row.protocol && named.indexOf(row.protocol) < 0) named.push(row.protocol);
    });
    if (!named.length) {
      box.innerHTML =
        '<span class="placeholder-note">no step runs a protocol, so this study carries none' +
        (sdProtocols.length
          ? " — " + sdProtocols.length + " available in this repo"
          : "") + "</span>";
      return;
    }
    box.innerHTML =
      '<div class="chip-pool">' +
      named
        .map(function (name) {
          var known = sdProtocols.filter(function (p) { return p.name === name; })[0];
          return (
            '<div class="probe-card"><div class="mono" style="font-size:12px;">' +
            escapeHtml(name) + "</div>" +
            '<div style="font-size:11px; color:var(--text-tertiary);">' +
            (known
              ? known.states.length + " states · " + escapeHtml(known.file) + ".eap"
              : "not declared by any .eap file in this repo") +
            "</div></div>"
          );
        })
        .join("") +
      "</div>" +
      (sdMaxProtocols != null && named.length > sdMaxProtocols
        ? '<p class="sd-error" style="margin-top:8px;">' + named.length +
          " distinct protocols — one study carries at most " + sdMaxProtocols + "</p>"
        : "");
  }

  /* Loads the file list. `sdEapFiles` is the whole editor's model. */
  async function sdEapLoad(selectStem) {
    var resp = await fetch("/api/study-designer/protocols");
    if (!resp.ok) {
      sdEapStatus(resp.status + " " + (await resp.text()), true);
      return;
    }
    var data = await resp.json();
    sdEapFiles = data.files || [];
    var dup = sdEl("sd-eap-duplicates");
    var dups = data.duplicate_names || [];
    dup.style.display = dups.length ? "block" : "none";
    dup.innerHTML = dups
      .map(function (d) {
        return (
          "Two files declare <span class=\"mono\">" + escapeHtml(d.name) +
          "</span>: " + escapeHtml(d.files.join(".eap, ")) +
          ".eap — a study naming it cannot be built until one is renamed."
        );
      })
      .join("<br>");
    renderEapFileList();
    var want = selectStem != null ? selectStem : sdEapStem;
    var found = sdEapFiles.filter(function (f) { return f.stem === want; })[0];
    sdEapSelect(found ? found.stem : (sdEapFiles[0] ? sdEapFiles[0].stem : null), true);
  }

  /* **Rendered once, then patched.** Re-rendering this list on every select
   * is the focus bug decision 17 records for the target dialog — it destroys
   * the node the click landed on. `syncEapFileList` moves the selection;
   * `renderEapFileList` is only for a list whose membership changed. */
  function renderEapFileList() {
    var list = sdEl("sd-eap-list");
    if (!list) return;
    if (!sdEapFiles.length) {
      list.innerHTML =
        '<p class="placeholder-note" style="padding:10px;">no .eap file in this repo yet</p>';
      return;
    }
    list.innerHTML = sdEapFiles
      .map(function (f) {
        return (
          '<button class="eap-file" data-eap-file="' + escapeHtml(f.stem) + '">' +
          escapeHtml(f.stem) + ".eap" +
          (f.errors.length ? " ⚠" : "") +
          '<div style="font-size:10.5px; color:var(--text-tertiary);">' +
          (f.errors.length
            ? "did not parse"
            : f.protocols.map(function (p) { return p.name; }).join(", ") || "no protocol") +
          "</div></button>"
        );
      })
      .join("");
    syncEapFileList();
  }

  function syncEapFileList() {
    var list = sdEl("sd-eap-list");
    if (!list) return;
    list.querySelectorAll("[data-eap-file]").forEach(function (node) {
      node.setAttribute(
        "aria-selected",
        node.getAttribute("data-eap-file") === sdEapStem ? "true" : "false"
      );
    });
  }

  /* Switching files is guarded: this dialog holds a file in the engineer's
   * repo, not a retypeable form, and nothing auto-saves. */
  function sdEapSelect(stem, force) {
    if (!force && sdEapDirty && stem !== sdEapStem) {
      if (!window.confirm("Discard unsaved changes to " + sdEapStem + ".eap?")) return;
    }
    sdEapStem = stem;
    sdEapDirty = false;
    var file = sdEapFiles.filter(function (f) { return f.stem === stem; })[0];
    // `|| ""` is not belt and braces: assigning `undefined` to a textarea's
    // `.value` yields the nine-character string "undefined", which parses as
    // a file and reports a syntax error on line 1. That is exactly what
    // shipped for one commit when the listing did not carry `text`.
    var text = (file && file.text) || "";
    sdEl("sd-eap-text").value = text;
    sdEapRenderErrors(file ? file.errors : [], false);
    sdEapSyncGutter();
    sdEapStatus(
      file
        ? (file.errors.length
            ? file.errors.length + " problem" + (file.errors.length === 1 ? "" : "s") +
              " in this file"
            : "parses · " + file.protocols.map(function (p) { return p.name; }).join(", "))
        : "no file selected",
      false
    );
    syncEapFileList();
  }

  function sdEapStatus(message, isError) {
    var el = sdEl("sd-eap-status");
    if (!el) return;
    el.className = isError ? "sd-error" : "placeholder-note";
    el.textContent = message;
  }

  /* The gutter is rebuilt from the text's own line count, so it and the
   * textarea cannot disagree about how many lines there are. Both scroll
   * together, driven by the textarea. */
  function sdEapSyncGutter() {
    var text = sdEl("sd-eap-text");
    var gutter = sdEl("sd-eap-gutter");
    if (!text || !gutter) return;
    var lines = text.value.split("\n").length;
    var out = [];
    for (var i = 1; i <= lines; i++) out.push(i);
    gutter.textContent = out.join("\n");
    gutter.scrollTop = text.scrollTop;
    sdEapPlaceBands();
  }

  /* Bands sit behind the text at `(line - 1) * --eap-line`, read from the
   * one place that variable is defined — so a change to the line height
   * moves the gutter, the text and the bands together. */
  function sdEapPlaceBands() {
    var text = sdEl("sd-eap-text");
    var bands = sdEl("sd-eap-bands");
    if (!text || !bands) return;
    var editor = text.closest(".eap-editor");
    var lineHeight = parseFloat(
      getComputedStyle(editor).getPropertyValue("--eap-line")
    ) || 18;
    var padding = parseFloat(getComputedStyle(text).paddingTop) || 0;
    bands.innerHTML = sdEapErrors
      // A line-0 error has no line to band — an unreadable file, or an error
      // about the file as a whole. It still lists below the editor.
      .filter(function (e) { return e.line > 0; })
      .map(function (e) {
        var top = padding + (e.line - 1) * lineHeight - text.scrollTop;
        return '<div class="eap-band" style="top:' + top + 'px;"></div>';
      })
      .join("");
  }

  /* `stale` is rendered, not hidden: a Check describes the text it was run
   * against, and one keystroke later it may describe a different file. */
  function sdEapRenderErrors(errors, stale) {
    sdEapErrors = errors || [];
    sdEl("sd-eap-bands").className = "eap-bands" + (stale ? " eap-stale" : "");
    sdEl("sd-eap-errors").innerHTML = sdEapErrors
      .map(function (e, i) {
        return (
          '<button class="eap-error" data-eap-error="' + i + '">' +
          escapeHtml(e.message) + "</button>"
        );
      })
      .join("");
    sdEapPlaceBands();
  }

  /* Clicking an error row puts the caret on its line. `setSelectionRange`
   * on the real textarea, which is the whole reason this is a textarea and
   * not a contenteditable renderer: native caret, undo, IME and clipboard
   * behaviour come free. */
  function sdEapGoToLine(line) {
    var text = sdEl("sd-eap-text");
    if (!text || line < 1) return;
    var lines = text.value.split("\n");
    var offset = 0;
    for (var i = 0; i < line - 1 && i < lines.length; i++) offset += lines[i].length + 1;
    text.focus();
    text.setSelectionRange(offset, offset + (lines[line - 1] || "").length);
  }

  async function sdEapCheck() {
    var resp = await fetch("/api/study-designer/protocols/check", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text: sdEl("sd-eap-text").value }),
    });
    if (!resp.ok) return sdEapStatus(resp.status + " " + (await resp.text()), true);
    var out = await resp.json();
    sdEapRenderErrors(out.errors || [], false);
    sdEapStatus(
      out.ok
        ? "parses · " + (out.protocols.map(function (p) { return p.name; }).join(", ") ||
            "no protocol declared")
        : out.errors.length + " problem" + (out.errors.length === 1 ? "" : "s") +
          " — nothing is written until this is clean",
      !out.ok
    );
  }

  async function sdEapSave() {
    if (!sdEapStem) return sdEapStatus("no file selected", true);
    var resp = await fetch(
      "/api/study-designer/protocols/" + encodeURIComponent(sdEapStem),
      {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text: sdEl("sd-eap-text").value }),
      }
    );
    var body = await resp.text();
    if (!resp.ok) {
      // The refusal carries the parser's own `line {n}: …`, so the band and
      // the message cannot disagree about where the problem is.
      var line = /line (\d+):/.exec(body);
      sdEapRenderErrors([{ message: body, line: line ? Number(line[1]) : 0 }], false);
      return sdEapStatus("not written — " + body, true);
    }
    sdEapDirty = false;
    await sdEapLoad(sdEapStem);
    // The row pickers read the same list, so a protocol saved here is
    // offerable immediately rather than after a reload.
    await loadSdActions();
    renderSdRows();
    sdEapStatus("saved", false);
  }

  async function sdEapDelete() {
    if (!sdEapStem) return;
    if (!window.confirm("Delete " + sdEapStem + ".eap?")) return;
    var resp = await fetch(
      "/api/study-designer/protocols/" + encodeURIComponent(sdEapStem),
      { method: "DELETE" }
    );
    var body = await resp.text();
    if (resp.status === 409) {
      return renderRefusal("sd-eap-refusal", resp.status, body);
    }
    if (!resp.ok) return sdEapStatus(resp.status + " " + body, true);
    sdEapDirty = false;
    sdEapStem = null;
    sdEl("sd-eap-refusal").style.display = "none";
    await sdEapLoad(null);
    await loadSdActions();
    renderSdRows();
  }

  function sdEapClose(force) {
    if (!force && sdEapDirty) {
      if (!window.confirm("Discard unsaved changes to " + sdEapStem + ".eap?")) return;
    }
    sdEapDirty = false;
    sdEl("sd-eap-dialog").style.display = "none";
    sdEl("sd-eap-backdrop").style.display = "none";
  }

  /* The two chip pools that give edit and delete somewhere to be launched
   * from — near-clones of `renderSdUnregistered`, which is the pool that
   * already turns a click into a dialog. */
  function renderSdRegistered() {
    var pool = sdEl("sd-registered");
    if (!pool) return;
    var actions = sdRegistry || [];
    if (!actions.length) {
      pool.innerHTML =
        '<span class="placeholder-note">no registered action in this repo yet — click a ' +
        "detected characteristic above to register one</span>";
      return;
    }
    pool.innerHTML = "";
    actions.forEach(function (action) {
      var chip = document.createElement("div");
      chip.className = "probe-card";
      chip.style.cursor = "pointer";
      var uuid = uuidStr(action.uuid);
      chip.innerHTML =
        '<div class="mono" style="font-size:12px;">' + escapeHtml(action.name) + "</div>" +
        '<div style="font-size:11px; color:var(--text-tertiary);">' +
        escapeHtml(action.operation) + " · " + escapeHtml(charLabel(uuid)) +
        ((action.fields || []).length ? " · " + action.fields.length + " field(s)" : "") +
        "</div>";
      chip.title = charTitle(uuid, uuidStr(action.service_uuid));
      chip.addEventListener("click", function () {
        openRegisterEdit(action.name);
      });
      pool.appendChild(chip);
    });
  }

  function renderSdLayouts() {
    var pool = sdEl("sd-layouts");
    if (!pool) return;
    if (!sdStructLayouts.length) {
      pool.innerHTML =
        '<span class="placeholder-note">no payload layout in this repo yet</span>';
      return;
    }
    pool.innerHTML = "";
    sdStructLayouts.forEach(function (layout) {
      var chip = document.createElement("div");
      chip.className = "probe-card";
      chip.style.cursor = "pointer";
      var header = (layout.header || []).length;
      var repeat = (layout.repeat || []).length;
      chip.innerHTML =
        '<div class="mono" style="font-size:12px;">' + escapeHtml(layout.name) + "</div>" +
        '<div style="font-size:11px; color:var(--text-tertiary);">' +
        header + " header · " + repeat + " repeat</div>";
      chip.addEventListener("click", function () {
        openLayoutDialog(layout.name);
      });
      pool.appendChild(chip);
    });
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
        // Remember whatever real version the field is holding *now*.
        // `dataset.stated || input.value` kept the first one ever stashed, so
        // ticking, unticking, retyping and ticking again restored the version
        // from two edits ago on the way back — silently replacing a version
        // requirement with a stale one, which is the one thing decision 11's
        // mandatory field exists to stop.
        if (input.value !== sdAnyLiteral) input.dataset.stated = input.value;
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
      build: sdBuildSpecPayload(),
      outpost: sdOutpostPayload(),
    };
  }

  // ---- the Build card (decision 11, reversed) -----------------------------
  //
  // What this tab can build, as the server reported it: the matched project,
  // the live target scan, and the flag names. Null until the first survey,
  // and `{available:false}` on a bench with no embarch-api config — which is
  // a state the card renders, not an error: the Study Designer works
  // perfectly well without a build toggle.
  var sdBuildSurvey = null;
  // The chosen snippets, **in order**. An array rather than a set of ticked
  // checkboxes, because west applies `-S` in order and reversals row 109 is
  // a case where the order decides whether the image works.
  var sdBuildSnippets = [];

  async function sdLoadBuildSurvey() {
    var note = sdEl("sd-build-note");
    try {
      var resp = await fetch("/api/build/survey");
      sdBuildSurvey = await resp.json();
    } catch (e) {
      sdBuildSurvey = { available: false, reason: String(e), flags: [] };
    }
    if (!sdBuildSurvey.available) {
      sdEl("sd-build-on").checked = false;
      sdEl("sd-build-on").disabled = true;
      sdEl("sd-build-body").style.display = "none";
      note.textContent = sdBuildSurvey.reason || "this bench cannot build the open repo";
      renderSdBuildOptsSummary();
      return;
    }
    sdEl("sd-build-on").disabled = false;
    note.textContent =
      "project " + sdBuildSurvey.project + ", from " + sdBuildSurvey.config_path +
      ". Off by default: flashing is the destructive half, and a study that only observes a " +
      "board somebody just flashed by hand must not silently overwrite it.";
    sdRenderBuildTargets();
    sdRenderBuildFlags();
    sdSyncBuildToggle();
    renderSdBuildOptsSummary();
  }

  // **One axis is the study's now, and it is the app** (decision 45).
  // Board, variant and revision come from the board type in the DUT role,
  // read at the moment a study runs, so the same saved study builds for
  // whatever is on the bench today — the same reason a study names a signal
  // and never a carrier. A blank first option is still "don't narrow",
  // which the resolver fills from the project's own default_target.
  function sdRenderBuildTargets() {
    var targets = (sdBuildSurvey.targets && sdBuildSurvey.targets.targets) || [];
    var axes = [["sd-build-app", "app"]];
    axes.forEach(function (axis) {
      var el = sdEl(axis[0]);
      var want = el.value;
      var seen = [];
      targets.forEach(function (t) {
        var v = t[axis[1]];
        if (v && seen.indexOf(v) === -1) seen.push(v);
      });
      seen.sort();
      el.innerHTML = "";
      var blank = document.createElement("option");
      blank.value = "";
      blank.textContent = "(the project's default)";
      el.appendChild(blank);
      seen.forEach(function (v) {
        var opt = document.createElement("option");
        opt.value = v;
        opt.textContent = v;
        el.appendChild(opt);
      });
      el.value = want;
      if (el.value !== want) el.value = "";
    });
    sdRenderRoleBoard();
    sdRenderSnippetPool();
  }

  // The read-out of what a run will build for: the board type in the DUT
  // role, resolved through the project's catalog the same way the server
  // resolves it. **Never a picker** — changing it is the Topology tab's
  // job, because it is a fact about the bench rather than about the study.
  function sdRenderRoleBoard() {
    var el = document.getElementById("sd-build-role-board");
    if (!el) return;
    var dut = latestSnapshot ? findEnrolled(latestSnapshot, "dut") : null;
    if (!dut || !dut.name) {
      el.textContent = "no board type in the DUT role — set one on the Topology tab";
      return;
    }
    var entry = boardCatalog.find(function (b) { return b.name === dut.name; });
    var target = entry && entry.build_target ? entry.build_target : dut.name;
    var extra = [];
    if (entry && entry.variant) extra.push("variant " + entry.variant);
    if (entry && entry.revision) extra.push("revision " + entry.revision);
    el.textContent = target + (extra.length ? " · " + extra.join(" · ") : "");
  }

  // The snippets the *chosen app* declares, from the same scan. An app with
  // none says so rather than showing an empty box.
  function sdRenderSnippetPool() {
    var byApp = (sdBuildSurvey && sdBuildSurvey.targets && sdBuildSurvey.targets.snippets_by_app) || {};
    var app = sdEl("sd-build-app").value;
    var pool = sdEl("sd-build-snippet-pool");
    pool.innerHTML = "";
    var names = app ? byApp[app] || [] : [];
    if (!app) {
      pool.textContent = "pick an app to see what it declares";
      return;
    }
    if (!names.length) {
      pool.textContent = "this app declares no snippets";
      return;
    }
    names.forEach(function (name) {
      var chip = document.createElement("span");
      chip.className = "chip";
      chip.textContent = "+ " + name;
      chip.title = "append to the ordered list";
      chip.addEventListener("click", function () {
        sdBuildSnippets.push(name);
        sdRenderChosenSnippets();
      });
      pool.appendChild(chip);
    });
    var appNote = sdEl("sd-build-app-note");
    var defaults = (sdBuildSurvey.targets && sdBuildSurvey.targets.default_snippets) || [];
    appNote.textContent = defaults.length
      ? "project default_snippets: " + defaults.join(", ")
      : "this project configures no default_snippets";
  }

  // The chosen list, with move-up / move-down / remove on each. **The
  // controls exist because the order is the statement**: a picker that
  // rendered these as a set would be showing a control that does nothing, on
  // top of a resolver that keeps the order.
  function sdRenderChosenSnippets() {
    var box = sdEl("sd-build-snippets");
    box.innerHTML = "";
    if (!sdBuildSnippets.length) {
      box.textContent = "none \u2014 the project's configured default_snippets are used";
      return;
    }
    sdBuildSnippets.forEach(function (name, i) {
      var chip = document.createElement("span");
      chip.className = "chip active-filter";
      chip.textContent = i + 1 + ". " + name + " ";
      ["\u2191", "\u2193", "\u00d7"].forEach(function (glyph, which) {
        var btn = document.createElement("a");
        btn.href = "#";
        btn.textContent = " " + glyph;
        btn.addEventListener("click", function (ev) {
          ev.preventDefault();
          if (which === 0 && i > 0) {
            var above = sdBuildSnippets[i - 1];
            sdBuildSnippets[i - 1] = sdBuildSnippets[i];
            sdBuildSnippets[i] = above;
          } else if (which === 1 && i < sdBuildSnippets.length - 1) {
            var below = sdBuildSnippets[i + 1];
            sdBuildSnippets[i + 1] = sdBuildSnippets[i];
            sdBuildSnippets[i] = below;
          } else if (which === 2) {
            sdBuildSnippets.splice(i, 1);
          }
          sdRenderChosenSnippets();
        });
        chip.appendChild(btn);
      });
      box.appendChild(chip);
    });
  }

  // One row per header flag, three states: don't care, must be set, must be
  // clear. Three and not two, because a flag's *clear* state can be the
  // requirement — `trace_self` clear is the standing example — and a
  // two-state control could not ask for it.
  function sdRenderBuildFlags() {
    var box = sdEl("sd-build-flags");
    box.innerHTML = "";
    ((sdBuildSurvey && sdBuildSurvey.flags) || []).forEach(function (name) {
      var row = document.createElement("div");
      row.className = "req-row";
      var label = document.createElement("div");
      label.className = "req-label mono";
      label.textContent = name;
      row.appendChild(label);
      var choices = document.createElement("div");
      choices.style.display = "flex";
      choices.style.gap = "12px";
      [["", "don't care"], ["set", "must be set"], ["clear", "must be clear"]].forEach(function (c) {
        var wrap = document.createElement("label");
        wrap.className = "req-any";
        var radio = document.createElement("input");
        radio.type = "radio";
        radio.name = "sd-build-flag-" + name;
        radio.value = c[0];
        radio.checked = c[0] === "";
        wrap.appendChild(radio);
        var span = document.createElement("span");
        span.textContent = c[1];
        wrap.appendChild(span);
        choices.appendChild(wrap);
      });
      row.appendChild(choices);
      box.appendChild(row);
    });
  }

  function sdSyncBuildToggle() {
    var on = sdEl("sd-build-on").checked && sdBuildSurvey && sdBuildSurvey.available;
    sdEl("sd-build-body").style.display = on ? "" : "none";
  }

  function sdBuildSpecPayload() {
    if (!sdEl("sd-build-on").checked || !sdBuildSurvey || !sdBuildSurvey.available) return null;
    var args = sdEl("sd-build-args").value
      .split("\n")
      .map(function (a) { return a.trim(); })
      .filter(function (a) { return a.length > 0; });
    return {
      // No board/variant/revision: the DUT role's board type supplies
      // them at run time (decision 45), so a study that stored one would
      // be stating a bench fact it has no business storing.
      app: sdEl("sd-build-app").value || null,
      snippets: sdBuildSnippets.slice(),
      extra_args: args,
    };
  }

  // **Sent whether or not the build toggle is on.** A mode requirement is a
  // statement about the firmware a study needs, not about who built it: a
  // study can perfectly well refuse to run against a DUT in the wrong mode
  // without building anything itself.
  function sdOutpostPayload() {
    var set = [];
    var clear = [];
    ((sdBuildSurvey && sdBuildSurvey.flags) || []).forEach(function (name) {
      var picked = document.querySelector(
        'input[name="sd-build-flag-' + name + '"]:checked'
      );
      if (!picked || !picked.value) return;
      (picked.value === "set" ? set : clear).push(name);
    });
    if (!set.length && !clear.length) return null;
    return { set: set, clear: clear };
  }

  function sdApplyBuild(spec, outpost) {
    var on = !!spec;
    sdEl("sd-build-on").checked = on && sdBuildSurvey && sdBuildSurvey.available;
    if (spec) {
      // A study saved before decision 45 can still carry a board; it is
      // not offered for editing and not resaved, and the run announces
      // that the role's board won. Nothing here silently rewrites the
      // file — opening a study must not change it.
      sdEl("sd-build-app").value = spec.app || "";
      sdBuildSnippets = (spec.snippets || []).slice();
      sdEl("sd-build-args").value = (spec.extra_args || []).join("\n");
    } else {
      sdBuildSnippets = [];
      sdEl("sd-build-args").value = "";
    }
    sdRenderSnippetPool();
    sdRenderChosenSnippets();
    ((sdBuildSurvey && sdBuildSurvey.flags) || []).forEach(function (name) {
      var want = "";
      if (outpost && (outpost.set || []).indexOf(name) !== -1) want = "set";
      if (outpost && (outpost.clear || []).indexOf(name) !== -1) want = "clear";
      var radio = document.querySelector(
        'input[name="sd-build-flag-' + name + '"][value="' + want + '"]'
      );
      if (radio) radio.checked = true;
    });
    sdSyncBuildToggle();
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

  /* ---- the Build options dialog ----------------------------------------
   *
   * Three properties of the saved study — which builds it is for, the
   * firmware it builds and flashes, and the dev-bench log level it asks for
   * — used to take about a third of the page between the study toolbar and
   * the step table, every one of them set once per study and then read. They
   * are behind a button now, with the one-line summary below standing in for
   * them on the page: a modal that hid all three would otherwise make "what
   * does this study require?" a question you have to open a dialog to
   * answer.
   */

  function sdReqSummaryText(inputId, anyId) {
    if (sdEl(anyId).checked) return "any";
    var stated = sdEl(inputId).value.trim();
    return stated || "unstated";
  }

  function renderSdBuildOptsSummary() {
    var el = sdEl("sd-buildopts-summary");
    if (!el) return;
    var bits = [
      "dev-bench " + sdReqSummaryText("sd-req-bench", "sd-req-bench-any"),
      "DUT " + sdReqSummaryText("sd-req-dut", "sd-req-dut-any"),
    ];
    var spec = sdBuildSpecPayload();
    if (spec) {
      // The board is the DUT role's, not the study's (decision 45), so the
      // summary says which app and leaves the board to the read-out above
      // it — a summary naming a board the study does not carry would be
      // stating something the file does not say.
      var target = spec.app || "";
      bits.push(
        "builds " + (target || "the project's default app") +
        (spec.snippets.length ? " + " + spec.snippets.length + " snippet" + (spec.snippets.length === 1 ? "" : "s") : "")
      );
    } else {
      bits.push("no build — runs against whatever is on the board");
    }
    var outpost = sdOutpostPayload();
    if (outpost) {
      var modes = (outpost.set || []).map(function (f) { return f; })
        .concat((outpost.clear || []).map(function (f) { return "no " + f; }));
      bits.push("mode: " + modes.join(", "));
    }
    // The level the study *states*, never the one the picker happens to be
    // showing: an untouched picker shows the server's default without the
    // study asserting it, and a summary that read the select would turn that
    // into a claim the file does not make.
    bits.push("log level " + (sdLogLevel || "unstated (server default)"));
    el.textContent = bits.join("  ·  ");
  }

  function sdOpenBuildOpts() {
    sdEl("sd-buildopts-name").textContent = sdEl("sd-name").value || "untitled-study";
    sdEl("sd-buildopts-backdrop").style.display = "block";
    sdEl("sd-buildopts-dialog").style.display = "block";
  }

  function sdCloseBuildOpts() {
    sdEl("sd-buildopts-backdrop").style.display = "none";
    sdEl("sd-buildopts-dialog").style.display = "none";
    renderSdBuildOptsSummary();
  }

  /* ---- static firmware analysis ----------------------------------------
   *
   * Reading the firmware repo's own source for its GATT table was, until
   * now, only ever a side effect: it ran lazily the first time some other
   * panel needed a characteristic name, cached for the life of the open
   * project, and reported its failures to a server log nobody running this
   * UI is reading. This is the same extraction, asked for on purpose, with
   * what it found and why it found nothing both on screen.
   */

  function sdStaticNote(text, bad) {
    var el = sdEl("sd-static-note");
    if (!el) return;
    el.textContent = text;
    el.className = bad ? "sd-error" : "placeholder-note";
    el.style.margin = "10px 0 0";
  }

  /* The ATT properties byte, as bit names. Declared in the source, not
   * observed off a board — which the panel says in words, because a
   * property this list shows and a live discovery contradicts is a real
   * thing that happens and must not read as a measurement. */
  function sdPropNames(properties) {
    return [
      [0x02, "read"],
      [0x04, "write-no-resp"],
      [0x08, "write"],
      [0x10, "notify"],
      [0x20, "indicate"],
    ]
      .filter(function (bit) { return properties & bit[0]; })
      .map(function (bit) { return bit[1]; });
  }

  function renderSdStaticResult(data) {
    var box = sdEl("sd-static-result");
    box.innerHTML = "";
    /* A properties alias the source #defines twice under a #if resolves to
     * the union of its branches, because which Kconfig a given build used is
     * not a fact in the source. That union is a properties byte no build
     * actually compiles, so it is named here rather than left to look like
     * an ordinary reading. */
    (data.conditional_properties || []).forEach(function (cond) {
      var note = document.createElement("p");
      note.className = "placeholder-note";
      note.style.margin = "0 0 10px";
      note.innerHTML =
        '<span class="mono">' + escapeHtml(cond.name) + "</span> is declared under a " +
        '<span class="mono">#if</span> as ' +
        cond.branches.map(function (b) { return escapeHtml(sdPropNames(b).join(" · ")); }).join(" or ") +
        " — read here as all of them (" + escapeHtml(sdPropNames(cond.used).join(" · ")) +
        "), since the build's own config is not in the source.";
      box.appendChild(note);
    });
    if (!data.services.length) return;
    data.services.forEach(function (service) {
      var card = document.createElement("div");
      card.className = "sd-static-service";
      var head = document.createElement("div");
      head.className = "sd-static-service-name";
      head.innerHTML =
        (service.name ? escapeHtml(service.name.label) + " " : "") +
        '<span class="mono placeholder-note">' + escapeHtml(service.uuid) + "</span>";
      if (service.name) head.title = service.name.origin;
      card.appendChild(head);
      service.characteristics.forEach(function (chrc) {
        var row = document.createElement("div");
        row.className = "sd-static-chrc";
        var props = sdPropNames(chrc.properties);
        row.innerHTML =
          "<span>" + (chrc.name ? escapeHtml(chrc.name.label) : '<span class="placeholder-note">unnamed</span>') + "</span>" +
          '<span class="mono placeholder-note">' + escapeHtml(chrc.uuid) + "</span>" +
          '<span class="sd-static-props">' + (props.length ? escapeHtml(props.join(" · ")) : "no properties declared") + "</span>";
        if (chrc.name) row.title = chrc.name.origin;
        card.appendChild(row);
      });
      box.appendChild(card);
    });
  }

  async function sdRunStaticAnalysis() {
    var btn = sdEl("sd-static-run");
    var original = btn.textContent;
    btn.disabled = true;
    btn.textContent = "Reading source…";
    sdStaticNote("reading the firmware repo's source…", false);
    try {
      // No body: there is one extractor and it runs against whatever repo is
      // open. This button re-reads the source, nothing more.
      var resp = await fetch("/api/study-designer/static-analysis", { method: "POST" });
      var text = await resp.text();
      if (!resp.ok) {
        sdEl("sd-static-result").innerHTML = "";
        return sdStaticNote(resp.status + " " + text, true);
      }
      var data = JSON.parse(text);
      renderSdStaticResult(data);
      if (data.error) {
        sdStaticNote(data.extractor + " failed: " + data.error, true);
      } else {
        sdStaticNote(
          data.extractor + " read " + data.services.length +
            (data.services.length === 1 ? " service, " : " services, ") +
            data.characteristic_count +
            (data.characteristic_count === 1 ? " characteristic" : " characteristics") +
            " out of the source — declared, not observed off a board.",
          false
        );
      }
      // Every panel that renders a characteristic name resolves it through
      // this extraction, so they are all stale the moment it re-runs.
      if (sdEl("sd-body").style.display !== "none") {
        try {
          await loadSdActions();
        } catch (e) {
          /* the extraction itself is what this button reports on */
        }
      }
      sdLoadProject();
    } catch (e) {
      sdStaticNote(String(e), true);
    } finally {
      btn.disabled = false;
      btn.textContent = original;
    }
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
    // Prefilling writes into the requirement fields, so the line on the page
    // that reports them is stale until this has run.
    renderSdBuildOptsSummary();
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

  /* One GATT-notify tap row (`embarch-study-designer` decisions
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
        .map(function (layout) {
          var name = layout.name;
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

  /* The record-framing cell.
   *
   * **Only a `gatt_notify` tap gets controls.** An outpost trace is a raw
   * UART capture with no record structure this mechanism can find, and
   * `TapInput::Outpost` carries no `record_magic` at all — so the cell says
   * why it is empty rather than offering a control that would do nothing.
   * Decision 11's rule: a menu whose entries are all wrong is worse than no
   * menu.
   *
   * The magic goes through `parseBytes(text, mode)` unchanged — the same
   * parser the registration form uses. A magic is ASCII or literal bytes,
   * which is exactly that function's two modes; a third spelling here would
   * be a second byte grammar in one file. */
  function sdRecordFramingCell(tap, i) {
    if (tap.kind !== "gatt_notify") {
      return (
        '<td><span class="placeholder-note">not applicable — an outpost trace is a raw ' +
        "UART capture with no record structure to find</span></td>"
      );
    }
    var bytes = tap.record_magic || [];
    var over = sdMaxRecordMagicLen != null && bytes.length > sdMaxRecordMagicLen;
    var status;
    if (tap.magicError) {
      status = '<span class="sd-error">' + escapeHtml(tap.magicError) + "</span>";
    } else if (!bytes.length) {
      status = '<span class="placeholder-note">blank — this capture is not checked</span>';
    } else if (over) {
      // Named, never trimmed: a shortened magic finds different record
      // boundaries, which is a check that measures the wrong thing and
      // passes. The server refuses it too.
      status =
        '<span class="sd-error">' + bytes.length + " bytes — the wire allows " +
        sdMaxRecordMagicLen + ", and a magic is never shortened to fit</span>";
    } else {
      status =
        '<span class="placeholder-note">' + bytes.length + " of " +
        (sdMaxRecordMagicLen == null ? "?" : sdMaxRecordMagicLen) + " bytes</span>";
    }
    return (
      '<td><div style="display:flex; gap:4px;">' +
      '<select class="sd-input" style="flex:0 0 74px;" data-tap-magic-mode="' + i + '">' +
      '<option value="text"' + (tap.magicMode === "text" ? " selected" : "") + ">text</option>" +
      '<option value="hex"' + (tap.magicMode !== "text" ? " selected" : "") + ">bytes</option>" +
      "</select>" +
      '<input class="sd-input mono" style="flex:1 1 auto; min-width:0;" data-tap-magic="' + i +
      '" spellcheck="false" placeholder="' +
      (tap.magicMode === "text" ? "GWF1" : "0x47 0x57 0x46 0x31") + '" value="' +
      escapeHtml(tap.magicText || "") + '" /></div>' +
      '<div style="margin-top:4px;">' + status + "</div></td>"
    );
  }

  /* Bytes back to the hex spelling, for a tap loaded off disk.
   *
   * Hex rather than the text it may have been typed as — the same choice the
   * raw-payload load path makes, and for the same reason: the saved form is
   * bytes, and re-rendering them as text would be a guess about an encoding
   * the bytes no longer carry. */
  function magicTextFromBytes(bytes) {
    return (bytes || [])
      .map(function (b) { return "0x" + ("0" + b.toString(16)).slice(-2); })
      .join(" ");
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
          // Emitted here rather than inside each kind's own cell builder, so
          // a column can never be added to one kind and forgotten on the
          // other — which is a row with the wrong number of cells, and a
          // table that silently shears.
          sdRecordFramingCell(tap, i) +
          '<td style="text-align:right;"><button class="sd-icon-btn" data-tap-remove="' + i +
          '" title="Remove this tap">✕</button></td></tr>'
        );
      })
      .join("");
    sdEl("sd-taps-empty").style.display = sdTaps.length ? "none" : "block";
  }

  /* Parses one tap's magic out of what was typed, in the mode chosen.
   *
   * On a parse failure `record_magic` is emptied rather than left holding
   * the last good value: a tap whose text says one thing and whose bytes say
   * another is exactly the silent disagreement this whole mechanism exists
   * to catch. The error is shown in the cell and the study still submits —
   * as a tap with no check, which the cell says. */
  function sdApplyMagic(tap, text) {
    tap.magicText = text;
    if (!text.trim()) {
      tap.record_magic = [];
      tap.magicError = "";
      return sdRefreshMagicCell(tap);
    }
    try {
      tap.record_magic = parseBytes(text, tap.magicMode === "text" ? "text" : "hex");
      tap.magicError = "";
    } catch (e) {
      tap.record_magic = [];
      tap.magicError = e.message;
    }
    sdRefreshMagicCell(tap);
  }

  /* Rewrites just this tap's status line, without re-rendering the row —
   * re-rendering while someone is typing in it moves the caret to the end,
   * which is the focus bug decision 17 records for the target dialog. */
  function sdRefreshMagicCell(tap) {
    var index = sdTaps.indexOf(tap);
    if (index < 0) return;
    var input = sdEl("sd-taps").querySelector('[data-tap-magic="' + index + '"]');
    if (!input) return;
    var cell = input.closest("td");
    if (!cell) return;
    var status = cell.lastElementChild;
    if (!status) return;
    var bytes = tap.record_magic || [];
    if (tap.magicError) {
      status.innerHTML = '<span class="sd-error">' + escapeHtml(tap.magicError) + "</span>";
    } else if (!bytes.length) {
      status.innerHTML =
        '<span class="placeholder-note">blank — this capture is not checked</span>';
    } else if (sdMaxRecordMagicLen != null && bytes.length > sdMaxRecordMagicLen) {
      status.innerHTML =
        '<span class="sd-error">' + bytes.length + " bytes — the wire allows " +
        sdMaxRecordMagicLen + ", and a magic is never shortened to fit</span>";
    } else {
      status.innerHTML =
        '<span class="placeholder-note">' + bytes.length + " of " +
        (sdMaxRecordMagicLen == null ? "?" : sdMaxRecordMagicLen) + " bytes</span>";
    }
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
        // Sliced to `sdMaxStreamNameLen` — `limits::MAX_STREAM_NAME_LEN`,
        // served on the actions response rather than restated as a literal
        // here (task ui/003). A name is a file name in this crate, and a
        // long identifier would be refused by `build_study` at submit time
        // rather than here, where it was chosen. When the cap has not been
        // served yet (or a server old enough not to send it), this does not
        // slice at all: a wrong guessed cap would let a still-too-long name
        // through as confidently as a right one, and `build_study`'s refusal
        // at submit time is the correct answer for a name we can't check the
        // real length of yet — never a silently-wrong shorter name either.
        name: first
          ? (sdMaxStreamNameLen == null
              ? charLabel(first.characteristic_uuid)
              : charLabel(first.characteristic_uuid).slice(0, sdMaxStreamNameLen))
          : "notify",
        service_uuid: first ? first.service_uuid : "",
        characteristic_uuid: first ? first.characteristic_uuid : "",
        decoder: "",
        // Blank is no check, which is the honest state for a payload whose
        // framing nobody has declared — never a default magic.
        record_magic: [],
        magicMode: "hex",
        magicText: "",
        magicError: "",
      });
      renderSdTaps();
    });
    tbody.addEventListener("input", function (ev) {
      var i = ev.target.getAttribute("data-tap-name");
      if (i !== null) {
        sdTaps[Number(i)].name = ev.target.value;
        return;
      }
      var m = ev.target.getAttribute("data-tap-magic");
      if (m !== null) sdApplyMagic(sdTaps[Number(m)], ev.target.value);
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
        return;
      }
      var mm = ev.target.getAttribute("data-tap-magic-mode");
      if (mm !== null) {
        var t = sdTaps[Number(mm)];
        t.magicMode = ev.target.value;
        // Re-read the same text under the new mode rather than converting
        // the bytes: "GWF1" means four bytes as text and nothing at all as
        // hex, and silently rewriting what was typed would hide that.
        sdApplyMagic(t, t.magicText || "");
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

  /* What the pending run requires — the table's fields for an authored run,
   * the file's own `requires` for a stored one. */
  async function sdPendingRequires() {
    if (sdPendingRun.kind !== "stored") return sdRequiresPayload();
    var resp = await fetch(
      "/api/study-designer/studies/" + encodeURIComponent(sdPendingRun.slug) + "/summary"
    );
    if (!resp.ok) {
      sdShowBuildError(resp.status + " " + (await resp.text()));
      return null;
    }
    var st = await resp.json();
    return {
      dev_bench_version: st.requires.dev_bench_version,
      firmware_version: st.requires.firmware_version,
    };
  }

  /* One Run button in the dialog, two destinations. */
  function sdDispatchRun(allowMismatch) {
    if (sdPendingRun.kind === "stored") return sdSubmitStoredRun(allowMismatch);
    return sdSubmitRun(allowMismatch);
  }

  async function sdSubmitStoredRun(allowMismatch) {
    closeRunCheck();
    var slug = sdPendingRun.slug;
    var btn = sdEl("sd-run");
    btn.disabled = true;
    try {
      var resp = await fetch(
        "/api/study-designer/studies/" + encodeURIComponent(slug) + "/run",
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ allow_version_mismatch: !!allowMismatch }),
        }
      );
      var text = await resp.text();
      if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
      sdHandOffToLiveStudy(text);
    } finally {
      btn.disabled = false;
    }
  }

  /// A run leaves this tab the moment embarch-core accepts it.
  ///
  /// The Study Designer authors and saves; running and watching happen on the
  /// Live Study tab, which is subscribed to this study before this function
  /// is even called — `POST` registers the session server-side. So there is
  /// nothing to race here: switching tabs is a view change, not a handover.
  // Two shapes, because a run with a build phase in front of it cannot be
  // one request: `{study_id}` is a study Core already accepted, and
  // `{build_id}` is a build that has just started and whose study id does
  // not exist yet. The second lands on the Live Study tab first and attaches
  // to the run when the build's own stream says it started.
  function sdHandOffToLiveStudy(responseText) {
    var body;
    try {
      body = JSON.parse(responseText);
    } catch (e) {
      return;
    }
    if (body.build_id) {
      showTab("live-study");
      lsFollowBuild(body.build_id);
      return;
    }
    if (!body.study_id) return;
    showTab("live-study");
    lsOpenStudy(body.study_id, true);
  }

  function closeRunCheck() {
    sdEl("sd-runcheck-backdrop").style.display = "none";
    sdEl("sd-runcheck-dialog").style.display = "none";
  }

  // What this run will put on the board, said before it happens.
  //
  // **A stored study's own spec is not read here**, and that is a real gap
  // rather than an oversight: this dialog knows the table's build settings,
  // and a run-only file's are in the file. The line says which case it is
  // instead of quietly describing the wrong study's build.
  function sdRenderRunCheckBuild() {
    var el = sdEl("sd-runcheck-build");
    if (!el) return;
    if (sdPendingRun && sdPendingRun.kind === "stored") {
      el.textContent =
        "If this saved study carries a build spec, it is built and flashed before the run " +
        "and the build appears as its own card on the Live Study tab.";
      return;
    }
    var spec = sdBuildSpecPayload();
    if (!spec) {
      el.textContent =
        "No firmware is built for this run \u2014 the DUT is whatever is already on the board. " +
        "Turn on the Build card to build and flash it first.";
      return;
    }
    var bits = [];
    if (spec.board) bits.push(spec.board);
    if (spec.app) bits.push("app " + spec.app);
    if (spec.variant) bits.push(spec.variant);
    if (spec.revision) bits.push("rev " + spec.revision);
    bits.push(
      spec.snippets.length
        ? "snippets: " + spec.snippets.join(" \u2192 ")
        : "the project's default snippets"
    );
    el.textContent =
      "This run builds and flashes the DUT first \u2014 " + bits.join(", ") +
      ". The working tree is built as it stands and is never moved.";
  }

  // Decision 11: the mismatch is shown *before* the run, with both strings, so
  // the choice is made against the actual discrepancy rather than in the
  // abstract. Core's gate is still the enforcement point — this reports, it
  // does not decide.
  async function sdOpenRunCheck() {
    // A stored file's own `requires` rather than the table's: the whole
    // point of this dialog is the gap between what THIS study needs and what
    // the bench has, and the table's requirements belong to a different
    // study.
    var requires = await sdPendingRequires();
    if (!requires) return;
    var params = new URLSearchParams(requires).toString();
    var resp = await fetch("/api/study-designer/version-check?" + params);
    if (!resp.ok) {
      // No pre-flight read available: run and let Core's own gate answer,
      // rather than blocking on a check this tab could not perform.
      return sdDispatchRun(false);
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
    sdRenderRunCheckBuild();
    sdEl("sd-runcheck-backdrop").style.display = "block";
    sdEl("sd-runcheck-dialog").style.display = "block";
    // Fired after the dialog is up, not awaited before it: a pre-flight the
    // server cannot answer must not keep the version check off the screen.
    sdLoadRunCheckCaps();
  }

  /* The full advisory set for the study about to run, from `/preflight`.
   *
   * Server-built, because two of the three cannot be computed here at all —
   * a postcard wire length and the event arms of a resolved ProtocolDef.
   * A pre-flight that fails renders as **unknown**, never as "within caps",
   * and never blocks the Run button beside it. */
  async function sdLoadRunCheckCaps() {
    var box = sdEl("sd-runcheck-caps");
    if (!box) return;
    if (sdPendingRun.kind === "stored") {
      // `/preflight` builds from the table's rows, and the table is not this
      // study. Saying nothing beats reporting another study's capacity.
      box.innerHTML =
        '<p class="placeholder-note">capacity is not reported for a run-only file — ' +
        "/preflight builds from the step table, which is a different study.</p>";
      return;
    }
    box.innerHTML = '<p class="placeholder-note">checking capacity…</p>';
    var rows;
    try {
      rows = sdCollectRows();
    } catch (e) {
      box.innerHTML =
        '<p class="placeholder-note">capacity <strong>unknown</strong> — ' +
        escapeHtml(e.message) + "</p>";
      return;
    }
    var resp;
    try {
      resp = await fetch("/api/study-designer/preflight", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          name: sdEl("sd-name").value.trim() || "untitled-study",
          rows: rows,
          requires: sdRequiresPayload(),
          taps: sdTaps,
          dev_bench_log_level: sdLogLevelPayload(),
        }),
      });
    } catch (e) {
      box.innerHTML =
        '<p class="placeholder-note">capacity <strong>unknown</strong> — the pre-flight ' +
        "request did not complete. Run is not blocked.</p>";
      return;
    }
    if (!resp.ok) {
      box.innerHTML =
        '<p class="placeholder-note">capacity <strong>unknown</strong> — ' +
        escapeHtml(await resp.text()) + ". Run is not blocked.</p>";
      return;
    }
    var pre = await resp.json();
    var facts =
      pre.steps + " step" + (pre.steps === 1 ? "" : "s") + " · " +
      pre.taps + " tap" + (pre.taps === 1 ? "" : "s") + " · " +
      pre.record_checks + " record check" + (pre.record_checks === 1 ? "" : "s") + " · " +
      (pre.protocols.length
        ? pre.protocols.map(escapeHtml).join(", ") + " (" + pre.protocols_wire_len + " bytes)"
        : "no protocols") +
      " · log level " + escapeHtml(pre.dev_bench_log_level);
    var advisories = pre.advisories || [];
    box.innerHTML =
      '<p class="placeholder-note">' + facts + "</p>" +
      (advisories.length
        ? '<ul class="sd-caps-list">' +
          advisories.map(function (a) { return "<li>" + escapeHtml(a) + "</li>"; }).join("") +
          "</ul><p class=\"placeholder-note\">Advisory only — the bench in front of you may " +
          "be a different build, so none of this blocks the run.</p>"
        : "");
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
    sdPendingRun = { kind: "authored" };
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
          // Saved with the study, unlike the waiver below — decision 51
          // rejected making the level a property of anything but the study.
          dev_bench_log_level: sdLogLevelPayload(),
          // A run parameter, never a study field: a saved study must not carry
          // a waiver into every later re-read of its own results.
          allow_version_mismatch: !!allowMismatch,
        }),
      });
      var text = await resp.text();
      if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
      sdHandOffToLiveStudy(text);
    } catch (e) {
      return sdShowBuildError(String(e));
    } finally {
      btn.disabled = false;
    }
  }

  // --- projects (decision 14) --------------------------------------------
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
    return state.path + " — " + bits.join(" · ");
  }

  // The open project is the whole page's, not one tab's: the board catalog
  // and the saved benches on Topology, the studies and the Build card here,
  // and every "no project open" message in the server. So it is rendered in
  // **one** function, from **one** state object, onto three surfaces — the
  // sidebar's control, this tab's summary line, and the picker dialog's
  // fields. Two renderers of the same fact is how a footer comes to name a
  // repo a tab has already switched away from.
  var projectState = { path: null, recents: [] };

  // The directory the repo is, which is what a human calls it. The full path
  // is one line below it in the sidebar and in the dialog, so nothing is
  // hidden by shortening this.
  function projectShortName(path) {
    if (!path) return "none open";
    var parts = String(path).split(/[\\/]/).filter(Boolean);
    return parts.length ? parts[parts.length - 1] : path;
  }

  // The line under the name is **where** that repo is — its last two parent
  // directories — not the path again: the name line already carries the last
  // segment, and the sidebar is 236 px wide, so a full path there is a
  // string cut off at whichever end the CSS chooses. Two checkouts of one
  // firmware differ exactly here, which is what makes the parent the useful
  // half. The whole path is on the control's tooltip and in the dialog.
  //
  // Computed rather than left to `direction: rtl`, which truncates on the
  // right side but reorders an absolute path's leading `/` to the far end —
  // rendering a trailing slash that is not in the path.
  function projectPathDisplay(path) {
    if (!path) return "pick a firmware repo";
    var sep = String(path).indexOf("\\") >= 0 && String(path).indexOf("/") < 0 ? "\\" : "/";
    var parts = String(path).split(/[\\/]/).filter(Boolean);
    var parent = parts.slice(0, -1);
    if (!parent.length) return path;
    return (parent.length > 2 ? "…" + sep : sep) + parent.slice(-2).join(sep);
  }

  function renderProject(state) {
    projectState = state || { path: null, recents: [] };

    var button = document.getElementById("project-button");
    var name = document.getElementById("project-current-name");
    var path = document.getElementById("project-current-path");
    if (button && name && path) {
      name.textContent = projectShortName(state.path);
      path.textContent = projectPathDisplay(state.path);
      button.classList.toggle("is-empty", !state.path);
      button.title = state.path
        ? state.path + " — the firmware repo every tab on this page reads and writes"
        : "No project open. Pick a firmware repo — every tab on this page is relative to it.";
    }

    var current = sdEl("sd-project-current");
    if (current) {
      current.textContent = sdProjectSummary(state);
      current.className = state.path ? "placeholder-note mono" : "sd-error";
    }

    var summary = document.getElementById("project-summary");
    if (summary) {
      summary.textContent = sdProjectSummary(state);
      summary.className = state.path ? "placeholder-note mono" : "sd-error";
    }

    var select = document.getElementById("project-recents");
    if (select) {
      select.innerHTML = '<option value="">Recent projects…</option>';
      (state.recents || []).forEach(function (r) {
        var opt = document.createElement("option");
        opt.value = r.path;
        opt.textContent = r.path;
        select.appendChild(opt);
      });
    }

    if (state.path) {
      var field = document.getElementById("project-path");
      if (field && !field.value) field.value = state.path;
    }
  }

  function openProjectDialog() {
    var dialog = document.getElementById("project-dialog");
    if (!dialog) return;
    showError(document.getElementById("project-error"), null);
    var field = document.getElementById("project-path");
    if (field && !field.value && projectState.path) field.value = projectState.path;
    dialog.style.display = "block";
    document.getElementById("project-dialog-backdrop").style.display = "block";
    if (field) field.focus();
  }

  function closeProjectDialog() {
    var dialog = document.getElementById("project-dialog");
    if (!dialog) return;
    dialog.style.display = "none";
    document.getElementById("project-dialog-backdrop").style.display = "none";
  }

  async function sdLoadProject() {
    try {
      var resp = await fetch("/api/study-designer/project");
      var state = await resp.json();
      renderProject(state);
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

  async function sdOpenProject(path) {
    var err = document.getElementById("project-error");
    err.style.display = "none";
    var body = {
      path: path !== undefined ? path : document.getElementById("project-path").value,
    };
    var btn = document.getElementById("project-open");
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
      renderProject(state);
      closeProjectDialog();
      // Everything the previous project put on screen is about the previous
      // project: the table, the taps and the saved-study list. The server
      // drops its own per-project caches on the same switch. A *run* is not
      // a project's — a study already in flight keeps running and keeps
      // being watched on the Live Study tab.
      sdRows = [];
      sdTaps = [];
      // Read out of the *previous* repo's source, down to the C identifiers
      // behind the names — the server drops its own copy on the same switch.
      sdEl("sd-static-result").innerHTML = "";
      sdStaticNote("not run yet against this project.", false);
      await sdEnterProject();
      // The board catalog, the saved benches and both role pickers are the
      // *project's*, so a switch that left them on screen would be showing
      // one repo's bench under another repo's name. Reloaded here rather
      // than on the next visit to the Topology tab, which may never come.
      await loadBoardCatalog();
      await loadProfiles();
    } catch (e) {
      err.textContent = String(e);
      err.style.display = "block";
    } finally {
      btn.disabled = false;
    }
  }

  /* "New study" means *start from blank*, and the Study name box is not
   * where that name comes from — it holds the name of the study that is
   * **open**. Sending it asked the server to create a file that already
   * existed, and the answer was a `409` naming a study the author had just
   * been reading: "'alpha-study' already exists — open it, or pick another
   * name", about a decision nobody had made.
   *
   * So a name is sent only when somebody typed one: the box differing from
   * what it was last filled with is exactly that test. Otherwise the server
   * is asked for the first free `untitled-study`, which cannot refuse. A
   * typed name that really does collide still gets the refusal, because
   * that one is about somebody's existing work.
   */
  async function sdNewStudy() {
    var typed = (sdEl("sd-name").value || "").trim();
    var stated = typed && typed !== sdLoadedName;
    var btn = sdEl("sd-new-study");
    btn.disabled = true;
    try {
      var resp = await fetch("/api/study-designer/new-study", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(
          stated ? { name: typed } : { name: "untitled-study", unique: true }
        ),
      });
      var text = await resp.text();
      if (!resp.ok) return sdShowBuildError(resp.status + " " + text);
      // The file the server wrote is already a valid, runnable, loadable
      // `Study`, so **the new study is loaded rather than approximated**:
      // what is on screen is what is on disk, byte for byte, and "new" and
      // "load" cannot drift apart because new *is* a load. This used to
      // reset the two tables by hand and leave every other panel showing the
      // study that was open a moment ago.
      var slug = JSON.parse(text).slug;
      await loadSdStudies();
      sdEl("sd-load-select").value = slug;
      sdCloseStored();
      await sdLoadStudy(slug);
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
    // Wired before the first paint: `sdAdoptActions` renders rows whose
    // listeners live on the table, and the template below is the first
    // thing a fresh project shows.
    if (!sdWired) {
      sdWireStudyDesigner();
      sdWired = true;
    }
    if (!sdRows.length) sdRows = sdCaptureTemplate();
    sdAdoptActions(data);
    loadSdStudies();
    // Prefilled from live bench state on first paint, which is what makes a
    // mandatory field a help rather than a tax.
    sdLoadBenchState(true);
    // And what this bench can build, which decides whether the Build card is
    // a card or an explanation of why it is not one.
    sdLoadBuildSurvey();
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
      // The target scan is off the same bench and goes as stale as the
      // versions do — a west workspace that gained a board since this tab
      // was opened is exactly the case decision 12's live scan exists for.
      sdLoadBuildSurvey();
    });
    sdEl("sd-build-on").addEventListener("change", sdSyncBuildToggle);
    sdEl("sd-build-app").addEventListener("change", function () {
      // The snippet pool is per-app, so changing the app changes what is
      // offerable. Chosen snippets are **left alone**: dropping them would
      // silently discard an authored ordering on a mis-click, and one that
      // the new app does not declare is refused by name at build time.
      sdRenderSnippetPool();
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
      sdDispatchRun(sdEl("sd-runcheck-allow").checked);
    });
    initSdTaps();
    sdEl("sd-new-study").addEventListener("click", sdNewStudy);
    sdEl("sd-save").addEventListener("click", sdSaveStudy);
    sdEl("sd-log-level").addEventListener("change", function (e) {
      // Set only once an author picks: an untouched select must not turn
      // "not stated" into an explicit level the study then carries.
      sdLogLevel = e.target.value || null;
      renderSdLogLevelNote();
    });
    sdEl("sd-delete").addEventListener("click", sdDeleteStudy);
    sdEl("sd-buildopts-open").addEventListener("click", sdOpenBuildOpts);
    sdEl("sd-buildopts-close").addEventListener("click", sdCloseBuildOpts);
    sdEl("sd-buildopts-backdrop").addEventListener("click", sdCloseBuildOpts);
    // One listener on the dialog rather than one per control: every field in
    // here feeds the summary line on the page, and a summary that is only
    // recomputed on close is wrong for as long as the dialog is open.
    sdEl("sd-buildopts-dialog").addEventListener("change", renderSdBuildOptsSummary);
    sdEl("sd-buildopts-dialog").addEventListener("input", renderSdBuildOptsSummary);
    sdEl("sd-load-select").addEventListener("change", function (ev) {
      var slug = ev.target.value;
      var known = sdStudyIndex[slug];
      // A run-only file never reaches `sdLoadStudy`: that route answers 409
      // for exactly this case, and calling it to be told so would put an
      // error in front of somebody who did nothing wrong.
      if (slug && known && known.editable === false) return sdOpenStored(slug);
      sdCloseStored();
      sdLoadStudy(slug);
    });
    sdEl("sd-stored-panel").addEventListener("click", function (ev) {
      if (ev.target.id === "sd-stored-run") sdRunStored();
    });

    sdEl("sd-reg-op").addEventListener("change", syncRegFieldsVisibility);
    sdEl("sd-reg-add-field").addEventListener("click", function (ev) {
      ev.preventDefault();
      addRegField();
    });
    sdEl("sd-register-dialog").addEventListener("click", onRegisterDialogClick);
    sdEl("sd-reg-cancel").addEventListener("click", closeRegisterDialog);
    sdEl("sd-reg-save").addEventListener("click", submitRegistration);
    sdEl("sd-reg-delete").addEventListener("click", deleteRegistration);
    sdEl("sd-add-layout").addEventListener("click", function () {
      openLayoutDialog(null);
    });
    sdEl("sd-layout-cancel").addEventListener("click", closeLayoutDialog);
    sdEl("sd-layout-backdrop").addEventListener("click", closeLayoutDialog);
    sdEl("sd-layout-save").addEventListener("click", submitLayout);
    sdEl("sd-layout-delete").addEventListener("click", deleteLayout);
    sdEl("sd-layout-add-header").addEventListener("click", function () {
      sdEl("sd-layout-header").insertAdjacentHTML("beforeend", layoutFieldHtml(null));
    });
    sdEl("sd-layout-add-repeat").addEventListener("click", function () {
      sdEl("sd-layout-repeat").insertAdjacentHTML("beforeend", layoutFieldHtml(null));
    });
    sdEl("sd-layout-dialog").addEventListener("click", function (ev) {
      var btn = ev.target.closest('[data-layout="remove"]');
      if (!btn) return;
      ev.preventDefault();
      btn.closest(".sd-reg-value").remove();
    });

    // --- the .eap editor
    sdEl("sd-eap-open").addEventListener("click", async function () {
      sdEl("sd-eap-dialog").style.display = "block";
      sdEl("sd-eap-backdrop").style.display = "block";
      sdEl("sd-eap-refusal").style.display = "none";
      await sdEapLoad(sdEapStem);
    });
    // The backdrop is guarded exactly as the Close button is: this dialog
    // holds a file in a repo, and a stray click outside it must not be the
    // thing that loses an edit.
    sdEl("sd-eap-backdrop").addEventListener("click", function () { sdEapClose(false); });
    sdEl("sd-eap-close").addEventListener("click", function () { sdEapClose(false); });
    sdEl("sd-eap-check").addEventListener("click", sdEapCheck);
    sdEl("sd-eap-save").addEventListener("click", sdEapSave);
    sdEl("sd-eap-delete").addEventListener("click", sdEapDelete);
    sdEl("sd-eap-new").addEventListener("click", function () {
      var stem = (window.prompt("New .eap file name (no extension)") || "").trim();
      if (!stem) return;
      // Added to the model rather than written: nothing here auto-saves, so
      // a new file exists on disk only once Save writes text that parses.
      if (!sdEapFiles.some(function (f) { return f.stem === stem; })) {
        sdEapFiles.push({ stem: stem, text: "", protocols: [], errors: [] });
        renderEapFileList();
      }
      sdEapSelect(stem, false);
      sdEapDirty = true;
    });
    sdEl("sd-eap-list").addEventListener("click", function (ev) {
      var btn = ev.target.closest("[data-eap-file]");
      if (!btn) return;
      sdEapSelect(btn.getAttribute("data-eap-file"), false);
    });
    sdEl("sd-eap-errors").addEventListener("click", function (ev) {
      var btn = ev.target.closest("[data-eap-error]");
      if (!btn) return;
      var err = sdEapErrors[Number(btn.getAttribute("data-eap-error"))];
      if (err) sdEapGoToLine(err.line);
    });
    sdEl("sd-eap-text").addEventListener("input", function () {
      sdEapDirty = true;
      // One keystroke and the bands describe text that no longer exists.
      // Greyed rather than cleared: an error that was there a moment ago is
      // still information, and silently dropping it reads as "fixed".
      sdEl("sd-eap-bands").className = "eap-bands eap-stale";
      sdEapStatus("edited — the marks below describe the text you had at the last Check", false);
      sdEapSyncGutter();
    });
    sdEl("sd-eap-text").addEventListener("scroll", function () {
      sdEl("sd-eap-gutter").scrollTop = sdEl("sd-eap-text").scrollTop;
      sdEapPlaceBands();
    });
    document.addEventListener("keydown", function (ev) {
      if (ev.key !== "Escape") return;
      if (sdEl("sd-eap-dialog").style.display !== "block") return;
      sdEapClose(false);
    });
    sdEl("sd-register-backdrop").addEventListener("click", closeRegisterDialog);
  }

  function initStudyDesignerTab() {
    var body = sdEl("sd-body");
    if (!body) return;

    // This tab's own button opens the *same* dialog the sidebar's control
    // does — one picker, reachable from where you already are.
    sdEl("sd-project-toggle").addEventListener("click", openProjectDialog);
    // Wired here, not in `sdWireStudyDesigner`: both buttons live on the
    // project card, which is on screen before any project is open, and that
    // function does not run until one is. `Discover GATT` used to be on the
    // study toolbar inside `sd-body`, where that distinction never came up.
    sdEl("sd-static-run").addEventListener("click", sdRunStaticAnalysis);
    sdEl("sd-discover").addEventListener("click", sdDiscover);
    sdLoadProject();
    // **Surveyed even with no project open**, because "no project" is one of
    // the answers: the card would otherwise sit on its "checking…"
    // placeholder forever on a tab whose data path never runs, which reads
    // as a request that hung rather than as a state.
    sdLoadBuildSurvey();
  }

  // --- signal routes (decision 10, first half) ----------------------------
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
    // The origin picker holds the two roles. A signal declared before the
    // vocabulary closed can name something else, and that value gets its
    // own option rather than falling silently back to `dut` — re-declaring
    // a signal must not quietly re-point it at a different origin.
    const origin = existing ? existing.origin_role : "dut";
    const originSelect = sigEl("sig-origin");
    if (!Array.prototype.some.call(originSelect.options, (o) => o.value === origin)) {
      const extra = document.createElement("option");
      extra.value = origin;
      extra.textContent = origin + " (not a role)";
      originSelect.appendChild(extra);
    }
    originSelect.value = origin;
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
      origin_role: sigEl("sig-origin").value || "dut",
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

  // --- The board catalog, saved benches, and Validate topology -------------
  //
  // Three surfaces the Topology tab grew in decision 44, and one thing they
  // have in common: **none of them is a second owner of hardware state.**
  // The catalog and the saved benches are project files this binary reads
  // and writes; every enrolment, signal and link write still goes to Core
  // over HTTP+Bearer (decision 5), and validation is Core's own live checks
  // rendered, never re-derived here.

  let boardCatalog = [];

  // What this repo can actually build each catalog row for, as the server
  // grouped its own target scan (`GET /api/topology/boards`). `available:
  // false` carries the reason and is a *state*, not a failure: a repo with
  // no `embarch-api` project config is a repo whose boards are still boards.
  let boardBuilds = { available: false, reason: null, by_board: {} };

  function boardCatalogHas(name) {
    return boardCatalog.some((b) => b.name === name);
  }

  function topoEl(id) {
    return document.getElementById(id);
  }

  function showError(el, text) {
    if (!el) return;
    if (!text) {
      el.style.display = "none";
      el.textContent = "";
      return;
    }
    el.style.display = "block";
    el.textContent = text;
  }

  // One axis of a catalog row's build menu — its revisions or its variants.
  // **Not its apps** (decision 49): which apps a board type is in the tree
  // for is a fact about the tree, not about the bench, and a study names the
  // app it builds.
  //
  // **The chip and the west target used to be the two columns here, and
  // neither is a thing a human reads a bench list for**: a chip is a
  // property you set once and a west target restates the board's own name.
  // What is worth knowing is what this repo can *build* that board as, which
  // is the same menu the DUT picker offers — so the list and the picker are
  // two views of one scan rather than two descriptions of one row.
  //
  // Four states, each said differently, because they are four different
  // facts: the scan could not run at all, this board is not in the scan,
  // the board is in the scan and declares none of this axis, and here they
  // are. Folding any pair together would state something about the bench
  // nobody established.
  function boardBuildCell(board, axis) {
    if (!boardBuilds.available) {
      return '<span class="placeholder-note" title="' +
        escapeHtml(boardBuilds.reason || "this repo's targets could not be scanned") +
        '">not scanned</span>';
    }
    const entry = (boardBuilds.by_board || {})[board.name];
    if (!entry) {
      return '<span class="placeholder-note" title="Nothing in this repo\'s target scan builds ' +
        'for this board type. Press Rescan after adding it to the repo, or check the row\'s ' +
        '“Builds as”.">not in the scan</span>';
    }
    const values = entry[axis] || [];
    // Which one a build would use today, as `embarch/boards.toml` pins it.
    const pinned = axis === "revisions" ? board.revision : board.variant;
    let html = values
      .map(function (v) {
        return '<span class="build-chip' + (v && v === pinned ? " is-pinned" : "") + '">' +
          escapeHtml(v) + "</span>";
      })
      .join("");
    // A pin the scan no longer backs is shown, not dropped: it is what a
    // build for this role would ask for, and a list that hid it would be
    // silent about exactly the row that is about to fail.
    if (pinned && values.indexOf(pinned) === -1) {
      html += '<span class="build-chip is-stale" title="Pinned in embarch/boards.toml, but this ' +
        'repo\'s scan does not have it — a build for this board would be refused.">' +
        escapeHtml(pinned) + " ?</span>";
    }
    if (!html) {
      return '<span class="placeholder-note">none declared</span>';
    }
    return html;
  }

  function boardCatalogRows() {
    if (!boardCatalog.length) {
      return (
        '<tr><td colspan="5" class="placeholder-note">No board type in this project yet — ' +
        "add one, or Rescan.</td></tr>"
      );
    }
    return boardCatalog
      .map(function (b) {
        // The chip and the west target are still this row's, and still
        // editable — they are on the row's own tooltip and in its Edit
        // dialog rather than holding a column each.
        const detail = [
          b.chip ? "chip " + b.chip : "no chip set",
          b.build_target ? "builds as " + b.build_target : "no west target set",
        ].join(" · ");
        return (
          '<tr><td class="mono" title="' + escapeHtml(detail) + '">' + escapeHtml(b.name) + "</td>" +
          "<td>" + boardBuildCell(b, "revisions") + "</td>" +
          "<td>" + boardBuildCell(b, "variants") + "</td>" +
          "<td>" + (b.notes ? escapeHtml(b.notes) : '<span class="placeholder-note">—</span>') + "</td>" +
          '<td style="text-align:right; white-space:nowrap;">' +
          '<button class="btn" data-board-edit="' + escapeHtml(b.name) + '">Edit</button> ' +
          '<button class="btn btn-icon-danger" data-board-remove="' + escapeHtml(b.name) +
          '" title="Forget this board. Nothing is unenrolled — a role holding it keeps its ' +
          'name, which then shows as not in catalog.">&#10005;</button>' +
          "</td></tr>"
        );
      })
      .join("");
  }

  function renderBoardCatalog() {
    const body = topoEl("board-catalog-body");
    if (body) body.innerHTML = boardCatalogRows();
    if (typeof sdRenderRoleBoard === "function") sdRenderRoleBoard();
    // The pickers read the same catalog, so a board type added here is
    // offered on the diagram without a reload.
    loadRolePickers();
  }

  async function loadBoardCatalog() {
    const err = topoEl("board-catalog-error");
    const path = topoEl("board-catalog-path");
    try {
      const resp = await fetch("/api/topology/boards");
      const text = await resp.text();
      if (resp.status === 409) {
        // No project open. Not an error to shout about: the catalog is a
        // project file, and the sidebar's project picker is the way in —
        // which the message says.
        boardCatalog = [];
        boardBuilds = { available: false, reason: text, by_board: {} };
        showError(err, null);
        if (path) path.textContent = text;
        renderBoardCatalog();
        return;
      }
      if (!resp.ok) {
        showError(err, resp.status + " " + text);
        return;
      }
      const body = JSON.parse(text);
      boardCatalog = body.boards || [];
      boardBuilds = body.builds || { available: false, reason: null, by_board: {} };
      showError(err, null);
      if (path) path.textContent = body.path;
      renderBoardCatalog();
    } catch (e) {
      showError(err, String(e));
    }
  }

  function openBoardDialog(existing) {
    showError(topoEl("board-result"), null);
    topoEl("board-name").value = existing ? existing.name : "";
    topoEl("board-name").readOnly = !!existing;
    topoEl("board-chip").value = (existing && existing.chip) || "";
    topoEl("board-build-target").value = (existing && existing.build_target) || "";
    topoEl("board-variant").value = (existing && existing.variant) || "";
    topoEl("board-revision").value = (existing && existing.revision) || "";
    topoEl("board-notes").value = (existing && existing.notes) || "";
    topoEl("board-dialog").style.display = "block";
    topoEl("board-dialog-backdrop").style.display = "block";
  }

  function closeBoardDialog() {
    topoEl("board-dialog").style.display = "none";
    topoEl("board-dialog-backdrop").style.display = "none";
  }

  async function saveBoard() {
    const result = topoEl("board-result");
    const body = {
      name: topoEl("board-name").value.trim(),
      chip: topoEl("board-chip").value.trim(),
      build_target: topoEl("board-build-target").value.trim(),
      variant: topoEl("board-variant").value.trim(),
      revision: topoEl("board-revision").value.trim(),
      notes: topoEl("board-notes").value.trim(),
    };
    if (!body.name) {
      showError(result, "a board needs a name");
      return;
    }
    const resp = await fetch("/api/topology/boards", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const text = await resp.text();
    if (!resp.ok) {
      showError(result, resp.status + " " + text);
      return;
    }
    boardCatalog = JSON.parse(text).boards || [];
    renderBoardCatalog();
    closeBoardDialog();
  }

  async function removeBoard(name) {
    const resp = await fetch("/api/topology/boards/" + encodeURIComponent(name), {
      method: "DELETE",
    });
    const text = await resp.text();
    if (!resp.ok) {
      showError(topoEl("board-catalog-error"), resp.status + " " + text);
      return;
    }
    boardCatalog = JSON.parse(text).boards || [];
    showError(topoEl("board-catalog-error"), null);
    renderBoardCatalog();
  }

  async function rescanBoards() {
    const err = topoEl("board-catalog-error");
    const button = topoEl("board-rescan");
    if (button) button.disabled = true;
    try {
      const resp = await fetch("/api/topology/boards/rescan", { method: "POST" });
      const text = await resp.text();
      if (!resp.ok) {
        showError(err, resp.status + " " + text);
        return;
      }
      const body = JSON.parse(text);
      boardCatalog = body.boards || [];
      // Re-read rather than render what the rescan returned: a rescan adds
      // board types, and what each one can be built as is the half this
      // response does not carry.
      await loadBoardCatalog();
      await loadRolePickers();
      // Said out loud, both halves: what the scan added, and what is in the
      // file that the scan did not find — a board type on a branch that is
      // not checked out is kept, not deleted, so the count is worth seeing.
      const added = (body.added || []).length;
      const missing = (body.not_in_scan || []).length;
      showError(
        err,
        added === 0 && missing === 0
          ? null
          : (added ? "added " + added + " board type(s) from the scan. " : "nothing new in the scan. ") +
            (missing ? missing + " in this file are not in the scan — kept, not deleted." : "")
      );
    } catch (e) {
      showError(err, String(e));
    } finally {
      if (button) button.disabled = false;
    }
  }

  // ---- retracting a role ----
  //
  // The counterpart enrolling went without: until this existed, a board
  // enrolled by mistake — or under an invented role, before roles closed to
  // a fixed pair — stayed in Core's enrollment file for good.
  async function unenrollRole(role) {
    const label = roleLabel(role);
    if (!window.confirm("Clear the role '" + label + "'? The hardware is untouched.")) {
      return;
    }
    const resp = await fetch("/api/enrolled/" + encodeURIComponent(role), { method: "DELETE" });
    if (!resp.ok) {
      const text = await resp.text();
      showError(topoEl("topo-profile-error"), resp.status + " " + text);
      return;
    }
    showError(topoEl("topo-profile-error"), null);
    delete roleStatus[role];
  }

  // ---- Validate topology ----

  function checkRowHtml(check) {
    const badge =
      check.status === "pass"
        ? '<span class="badge badge-success">pass</span>'
        : check.status === "fail"
        ? '<span class="badge badge-danger">fail</span>'
        : check.status === "warn"
        ? '<span class="badge badge-warning">warn</span>'
        : '<span class="badge badge-neutral">empty</span>';
    // **A log is shown, never summarised.** When Core refused something,
    // its own words are what a human acts on; a paraphrase here would be a
    // second, worse description of a failure this UI did not diagnose.
    const log = check.log
      ? '<pre class="topo-check-log">' + escapeHtml(check.log) + "</pre>"
      : "";
    // **A leftover role is the one finding this report can act on.** It is
    // not drawn on the diagram — the picture has two boxes, and that row is
    // in neither — so without a control here the only surface that can see
    // it could not clear it, which is the gap decision 44 closed for the
    // roles table and the fold would otherwise have reopened.
    const action =
      check.id.indexOf("foreign:") === 0
        ? '<span style="flex:1;"></span><button class="btn btn-icon-danger" data-unenroll-role="' +
          escapeHtml(check.id.slice("foreign:".length)) + '" title="Clear this leftover role">&#10005;</button>'
        : "";
    return (
      '<div class="topo-check">' +
      '<div class="topo-check-head">' + badge +
      '<span class="topo-check-label">' + escapeHtml(check.label) + "</span>" + action + "</div>" +
      '<p class="placeholder-note" style="margin:4px 0 0;">' + escapeHtml(check.detail) + "</p>" +
      log +
      "</div>"
    );
  }

  async function validateTopology() {
    const report = topoEl("topo-validate-report");
    const button = topoEl("topo-validate");
    if (!report) return;
    report.style.display = "block";
    report.innerHTML = '<p class="placeholder-note">validating — each role is a live probe attach…</p>';
    if (button) button.disabled = true;
    try {
      const resp = await fetch("/api/topology/validate", { method: "POST" });
      const text = await resp.text();
      if (!resp.ok) {
        report.innerHTML =
          '<div class="sd-error">' + escapeHtml(resp.status + " " + text) + "</div>";
        return;
      }
      const body = JSON.parse(text);
      // Feed the diagram from the same answer the report is drawn from —
      // one source, two renderings, the rule the signal rows and the signal
      // lanes are already under.
      roleStatus = {};
      (body.checks || []).forEach(function (c) {
        if (c.id.indexOf("role:") !== 0) return;
        roleStatus[c.id.slice("role:".length)] = {
          status: c.status,
          at: formatTimestamp(body.checked_at_utc_ms),
          short: c.status === "empty" ? "nothing to check yet" : c.detail.split("—")[0].trim(),
        };
      });
      if (latestSnapshot) renderTopologyDiagram(latestSnapshot);
      const head =
        '<div class="topo-check-summary">' +
        (body.ok
          ? '<span class="badge badge-success">every check passed</span>'
          : '<span class="badge badge-danger">' + body.failed + " failed</span>") +
        (body.warned ? ' <span class="badge badge-warning">' + body.warned + " to look at</span>" : "") +
        ' <span class="placeholder-note">' + formatTimestamp(body.checked_at_utc_ms) + "</span></div>";
      report.innerHTML = head + (body.checks || []).map(checkRowHtml).join("");
    } catch (e) {
      report.innerHTML = '<div class="sd-error">' + escapeHtml(String(e)) + "</div>";
    } finally {
      if (button) button.disabled = false;
    }
  }

  // ---- saved benches ----

  let savedProfiles = [];

  function renderProfilePicker() {
    const picker = topoEl("topo-profile-picker");
    if (!picker) return;
    const previous = picker.value;
    if (!savedProfiles.length) {
      picker.innerHTML = '<option value="">no saved topology</option>';
      return;
    }
    picker.innerHTML = savedProfiles
      .map(function (p) {
        const label = p.error
          ? p.slug + " — unreadable"
          : p.name + " · " + formatTimestamp(p.saved_at_utc_ms);
        return '<option value="' + escapeHtml(p.slug) + '">' + escapeHtml(label) + "</option>";
      })
      .join("");
    if (previous) picker.value = previous;
  }

  async function loadProfiles() {
    const err = topoEl("topo-profile-error");
    try {
      const resp = await fetch("/api/topology/profiles");
      const text = await resp.text();
      if (resp.status === 409) {
        savedProfiles = [];
        renderProfilePicker();
        return;
      }
      if (!resp.ok) {
        showError(err, resp.status + " " + text);
        return;
      }
      savedProfiles = JSON.parse(text).profiles || [];
      showError(err, null);
      renderProfilePicker();
    } catch (e) {
      showError(err, String(e));
    }
  }

  function openSaveTopologyDialog() {
    showError(topoEl("topo-save-result"), null);
    topoEl("topo-save-name").value = topoEl("topo-profile-name").textContent === "unsaved"
      ? ""
      : topoEl("topo-profile-name").textContent;
    topoEl("topo-save-dialog").style.display = "block";
    topoEl("topo-save-backdrop").style.display = "block";
  }

  function closeSaveTopologyDialog() {
    topoEl("topo-save-dialog").style.display = "none";
    topoEl("topo-save-backdrop").style.display = "none";
  }

  async function saveTopology() {
    const name = topoEl("topo-save-name").value.trim();
    const result = topoEl("topo-save-result");
    if (!name) {
      showError(result, "a topology needs a name");
      return;
    }
    const resp = await fetch("/api/topology/profiles", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: name }),
    });
    const text = await resp.text();
    if (!resp.ok) {
      showError(result, resp.status + " " + text);
      return;
    }
    const body = JSON.parse(text);
    savedProfiles = body.profiles || [];
    renderProfilePicker();
    topoEl("topo-profile-picker").value = body.saved;
    topoEl("topo-profile-name").textContent = name;
    closeSaveTopologyDialog();
  }

  async function deleteTopology() {
    const slug = topoEl("topo-profile-picker").value;
    if (!slug) return;
    if (!window.confirm("Delete the saved topology '" + slug + "'? The bench itself is untouched.")) {
      return;
    }
    const resp = await fetch("/api/topology/profiles/" + encodeURIComponent(slug), {
      method: "DELETE",
    });
    const text = await resp.text();
    if (!resp.ok) {
      showError(topoEl("topo-profile-error"), resp.status + " " + text);
      return;
    }
    savedProfiles = JSON.parse(text).profiles || [];
    showError(topoEl("topo-profile-error"), null);
    renderProfilePicker();
  }

  // **Loading applies two halves and proposes the third.** Signals and the
  // dev-bench link are declarations, so they go straight in. An enrolment
  // is an identity claim — a role bound to a probe after a live hardware-ID
  // read — so each one comes back as a proposal with a button, and pressing
  // it runs the ordinary enroll. A file on disk is not evidence about what
  // is plugged in.
  function proposalHtml(p) {
    const notes = [];
    if (p.already_enrolled) notes.push("this role already holds that probe");
    if (!p.probe_attached) notes.push("that probe is not attached right now");
    if (p.displaces && p.displaces.probe_serial !== p.probe_serial) {
      notes.push(
        "replaces " + (p.displaces.board || "an unnamed board") + " on probe " + p.displaces.probe_serial
      );
    }
    return (
      '<div class="topo-check">' +
      '<div class="topo-check-head">' +
      '<span class="badge badge-neutral">' + escapeHtml(roleLabel(p.role)) + "</span>" +
      '<span class="topo-check-label mono">' + escapeHtml(p.board || "unnamed board") + "</span>" +
      '<span style="flex:1;"></span>' +
      '<button class="btn btn-primary" data-proposal-role="' + escapeHtml(p.role) + '">Enroll</button>' +
      "</div>" +
      '<p class="placeholder-note" style="margin:4px 0 0;">chip ' + escapeHtml(p.chip) +
      " · probe " + escapeHtml(p.probe_serial) +
      (notes.length ? " · " + escapeHtml(notes.join(" · ")) : "") + "</p>" +
      "</div>"
    );
  }

  let pendingProposals = [];

  async function applyTopology() {
    const slug = topoEl("topo-profile-picker").value;
    const report = topoEl("topo-apply-report");
    if (!slug || !report) return;
    report.style.display = "block";
    report.innerHTML = '<p class="placeholder-note">loading…</p>';
    const resp = await fetch("/api/topology/profiles/" + encodeURIComponent(slug) + "/apply", {
      method: "POST",
    });
    const text = await resp.text();
    if (!resp.ok) {
      report.innerHTML = '<div class="sd-error">' + escapeHtml(resp.status + " " + text) + "</div>";
      return;
    }
    const body = JSON.parse(text);
    pendingProposals = body.proposals || [];
    topoEl("topo-profile-name").textContent = body.name;
    const applied = (body.applied || [])
      .map(function (a) {
        const badge = a.ok
          ? '<span class="badge badge-success">applied</span>'
          : '<span class="badge badge-warning">not applied</span>';
        return (
          '<div class="topo-check"><div class="topo-check-head">' + badge +
          '<span class="topo-check-label">' + escapeHtml(a.what) + "</span></div>" +
          '<p class="placeholder-note" style="margin:4px 0 0;">' + escapeHtml(a.detail) + "</p></div>"
        );
      })
      .join("");
    const proposals = pendingProposals.map(proposalHtml).join("");
    report.innerHTML =
      '<div class="topo-check-summary"><span class="badge badge-neutral">loaded ' +
      escapeHtml(body.name) + "</span> " +
      '<span class="placeholder-note">the declarations are in; each enrolment is yours to confirm, ' +
      "because binding a role to a probe reads that chip&rsquo;s live hardware ID</span></div>" +
      applied +
      proposals;
  }

  async function confirmProposal(role) {
    const p = pendingProposals.find(function (x) {
      return x.role === role;
    });
    if (!p) return;
    const report = topoEl("topo-apply-report");
    const resp = await fetch("/api/enroll", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        role: p.role,
        chip: p.chip,
        probe_serial: p.probe_serial,
        name: p.board,
      }),
    });
    const text = await resp.text();
    if (!resp.ok) {
      showError(topoEl("topo-profile-error"), resp.status + " " + text);
      return;
    }
    showError(topoEl("topo-profile-error"), null);
    // dev-bench's link is amended onto its enrolment row, so Core refuses
    // it while the role is empty — which is exactly the state the apply
    // step deferred it from. Now that the row exists, declare it.
    if (p.role === "dev-bench" && (p.link_port_serial || p.link_port_interface !== null)) {
      await fetch("/api/topology/link", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ serial: p.link_port_serial, interface: p.link_port_interface }),
      });
    }
    pendingProposals = pendingProposals.filter(function (x) {
      return x.role !== role;
    });
    if (report) {
      const remaining = pendingProposals.map(proposalHtml).join("");
      report.innerHTML =
        '<div class="topo-check-summary"><span class="badge badge-success">enrolled ' +
        escapeHtml(roleLabel(role)) + "</span></div>" + remaining;
    }
  }

  // ---- picking the board type in a role ----
  //
  // Two lists with two owners (decision 45): the DUT's comes from the open
  // project's catalog, the dev bench's from the suite's supported set.
  // Both are **served**, never restated here — the same rule every other
  // vocabulary in this tab is under.
  let rolePickers = { dut: [], "dev-bench": [], builds: { available: false, by_board: {} } };

  // A dev-bench board type is a west qualifier — `nrf54l15dk/nrf54l15/cpuapp`
  // — and that is what Core stores and what a build is for, so it stays the
  // value everywhere it is *sent*. It is not what a human reads on a
  // diagram, though: the picture answers "which board is on the desk", and a
  // slash-separated path answers a different question. The label comes from
  // the served supported list (decision 45), never from a table typed here,
  // and a type the list does not carry renders unchanged.
  function boardTypeLabel(role, name) {
    if (!name) return name;
    if (role !== "dev-bench") return name;
    const found = (rolePickers["dev-bench"] || []).find((o) => o.board === name);
    return found && found.label ? found.label : name;
  }

  async function loadRolePickers() {
    try {
      const resp = await fetch("/api/topology/pickers");
      if (!resp.ok) return;
      rolePickers = await resp.json();
    } catch (e) {
      /* the picker falls back to whatever it last had; the box still draws */
    }
  }

  // How one scanned combination reads in the picker: **the two axes it is
  // being chosen between, and nothing after them** (decision 49). The west
  // qualifier used to close every line and the apps followed it, which made
  // a list of `nrf54l15dk@0.9.0/nrf54l15/cpuapp/ns` differing from its
  // neighbour in one character — the string the picker exists to spare a
  // human — and repeated an app list that this dialog decides nothing about.
  // The qualifier is on the option's tooltip, where a value you check once
  // belongs, the way a dev-bench board type keeps its own.
  function comboLabel(combo) {
    const bits = [];
    bits.push(combo.revision ? "rev " + combo.revision : "no revision");
    bits.push(combo.variant ? "variant " + combo.variant : "no variant");
    return bits.join(" · ");
  }

  // The combinations offered for whatever board type is selected right now.
  //
  // **Only combinations the repo's own scan reports**, never a revision list
  // crossed with a variant list: a revision can be backed only *with* a
  // named variant and a variant only at one revision, so a cross product
  // offers targets `west build` then refuses. Where there is nothing to
  // offer — a dev-bench board, a repo that cannot be scanned, a board type
  // the scan does not have — the field goes away and the note says which of
  // those it is, because "no combinations" and "could not look" are not the
  // same sentence.
  function renderRoleCombos(role) {
    const field = topoEl("role-board-combo-field");
    const select = topoEl("role-board-combo");
    const note = topoEl("role-board-combo-note");
    if (!field || !select || !note) return;
    const builds = rolePickers.builds || { available: false, by_board: {} };
    const board = topoEl("role-board-select").value;
    const entry = role === "dut" && builds.available ? (builds.by_board || {})[board] : null;
    const combos = entry ? entry.combos || [] : [];

    if (!combos.length) {
      field.style.display = "none";
      select.innerHTML = "";
      let text = "";
      if (role === "dut" && !builds.available) {
        text = "This repo's build targets could not be scanned, so no combination is offered " +
          "and the board type is set on its own." + (builds.reason ? " " + builds.reason : "");
      } else if (role === "dut" && board) {
        text = "Nothing in this repo's target scan builds for this board type — press Rescan " +
          "in Board types below, or check what it builds as.";
      }
      note.textContent = text;
      note.style.display = text ? "" : "none";
      return;
    }

    const picked = (rolePickers[role] || []).find((o) => o.board === board) || {};
    select.innerHTML = combos
      .map(function (c, i) {
        return '<option value="' + i + '" title="' + escapeHtml(c.qualifier) + '">' +
          escapeHtml(comboLabel(c)) + "</option>";
      })
      .join("");
    // Opens on what `embarch/boards.toml` already pins for this board, so
    // confirming the dialog is not silently a change of target.
    const current = combos.findIndex(function (c) {
      return c.revision === (picked.revision || "") && c.variant === (picked.variant || "");
    });
    select.value = String(current >= 0 ? current : 0);
    field.style.display = "";
    // **Only the two states that are a refusal keep a note.** Saying "3
    // combinations in this repo's scan" beside a list of three is the list
    // read aloud; what could not be scanned, and what the scan does not
    // have, are the two a human cannot see from the select itself.
    note.textContent = "";
    note.style.display = "none";
  }

  function openBoardPicker(role) {
    const options = rolePickers[role] || [];
    const dialog = topoEl("role-board-dialog");
    if (!dialog) return;
    topoEl("role-board-title").textContent = "Board type in " + roleLabel(role);
    const select = topoEl("role-board-select");
    if (!options.length) {
      select.innerHTML =
        '<option value="">' +
        (role === "dut"
          ? "no board type in this project yet — add or rescan below"
          : "no supported dev-bench board") +
        "</option>";
    } else {
      select.innerHTML = options
        .map(function (o) {
          return '<option value="' + escapeHtml(o.board) + '" data-chip="' + escapeHtml(o.chip) +
            '">' + escapeHtml(o.label) + (o.chip ? " — " + escapeHtml(o.chip) : "") + "</option>";
        })
        .join("");
    }
    const current = findEnrolled(latestSnapshot || {}, role);
    if (current && current.name) select.value = current.name;
    renderRoleCombos(role);
    showError(topoEl("role-board-result"), null);
    dialog.dataset.role = role;
    dialog.style.display = "block";
    topoEl("role-board-backdrop").style.display = "block";
  }

  function closeBoardPicker() {
    topoEl("role-board-dialog").style.display = "none";
    topoEl("role-board-backdrop").style.display = "none";
  }

  async function retractRole() {
    const dialog = topoEl("role-board-dialog");
    const role = dialog.dataset.role;
    if (!window.confirm(
      "Retract " + roleLabel(role) + "? Its board type and its probe binding both go; the " +
        "hardware is untouched."
    )) {
      return;
    }
    const resp = await fetch("/api/enrolled/" + encodeURIComponent(role), { method: "DELETE" });
    if (!resp.ok && resp.status !== 404) {
      showError(topoEl("role-board-result"), resp.status + " " + (await resp.text()));
      return;
    }
    delete roleStatus[role];
    closeBoardPicker();
  }

  async function saveRoleBoard() {
    const dialog = topoEl("role-board-dialog");
    const role = dialog.dataset.role;
    const select = topoEl("role-board-select");
    const board = select.value;
    const chip = select.selectedOptions.length
      ? select.selectedOptions[0].getAttribute("data-chip") || ""
      : "";
    if (!board) {
      showError(topoEl("role-board-result"), "there is no board type to pick yet");
      return;
    }
    // One request, so the role and the combination cannot half-apply. The
    // combination is sent only when one was actually offered: a request that
    // named an empty revision and variant would *clear* the board's pin,
    // which is a decision nobody made by picking a board type.
    const body = { board: board, chip: chip };
    const comboField = topoEl("role-board-combo-field");
    if (comboField && comboField.style.display !== "none") {
      const builds = rolePickers.builds || { by_board: {} };
      const entry = (builds.by_board || {})[board];
      const combo = entry && entry.combos ? entry.combos[Number(topoEl("role-board-combo").value)] : null;
      if (combo) {
        body.revision = combo.revision;
        body.variant = combo.variant;
      }
    }
    const resp = await fetch("/api/topology/roles/" + encodeURIComponent(role) + "/board", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!resp.ok) {
      showError(topoEl("role-board-result"), resp.status + " " + (await resp.text()));
      return;
    }
    // The pin the server just wrote is on the catalog row, which the list
    // above and the picker both render from.
    await loadBoardCatalog();
    await loadRolePickers();
    // The board in this role changed, so whatever the last Validate said
    // about it describes a different board. Dropped rather than kept: a
    // stale badge is a claim nobody made.
    delete roleStatus[role];
    closeBoardPicker();
  }

  function initTopologyTab() {
    const addBoard = topoEl("board-add");
    if (addBoard) addBoard.addEventListener("click", () => openBoardDialog(null));
    const boardCancel = topoEl("board-cancel");
    if (boardCancel) boardCancel.addEventListener("click", closeBoardDialog);
    const boardBackdrop = topoEl("board-dialog-backdrop");
    if (boardBackdrop) boardBackdrop.addEventListener("click", closeBoardDialog);
    const boardSave = topoEl("board-save");
    if (boardSave) boardSave.addEventListener("click", saveBoard);

    const catalogBody = topoEl("board-catalog-body");
    if (catalogBody) {
      catalogBody.addEventListener("click", function (ev) {
        const edit = ev.target.closest("[data-board-edit]");
        if (edit) {
          const name = edit.getAttribute("data-board-edit");
          openBoardDialog(boardCatalog.find((b) => b.name === name) || { name: name });
          return;
        }
        const remove = ev.target.closest("[data-board-remove]");
        if (remove) {
          const name = remove.getAttribute("data-board-remove");
          if (
            window.confirm(
              "Forget the board '" + name + "'? Nothing is unenrolled — a role holding it keeps " +
                "the name, which then shows as not in catalog."
            )
          ) {
            removeBoard(name);
          }
        }
      });
    }

    const rolesBody = topoEl("topology-table-body");
    if (rolesBody) {
      rolesBody.addEventListener("click", function (ev) {
        const btn = ev.target.closest("[data-unenroll-role]");
        if (btn) unenrollRole(btn.getAttribute("data-unenroll-role"));
      });
    }

    const validate = topoEl("topo-validate");
    if (validate) validate.addEventListener("click", validateTopology);

    const save = topoEl("topo-profile-save");
    if (save) save.addEventListener("click", openSaveTopologyDialog);
    const saveCancel = topoEl("topo-save-cancel");
    if (saveCancel) saveCancel.addEventListener("click", closeSaveTopologyDialog);
    const saveBackdrop = topoEl("topo-save-backdrop");
    if (saveBackdrop) saveBackdrop.addEventListener("click", closeSaveTopologyDialog);
    const saveConfirm = topoEl("topo-save-confirm");
    if (saveConfirm) saveConfirm.addEventListener("click", saveTopology);
    const load = topoEl("topo-profile-load");
    if (load) load.addEventListener("click", applyTopology);
    const del = topoEl("topo-profile-delete");
    if (del) del.addEventListener("click", deleteTopology);

    const applyReport = topoEl("topo-apply-report");
    if (applyReport) {
      applyReport.addEventListener("click", function (ev) {
        const btn = ev.target.closest("[data-proposal-role]");
        if (btn) confirmProposal(btn.getAttribute("data-proposal-role"));
      });
    }

    const svg = document.getElementById("topology-diagram");
    if (svg) {
      // Delegated, like the drop target it sits on: the whole picture is
      // rebuilt on every snapshot, so a listener bound to a node here would
      // be gone within five seconds (decision 43's first load-bearing
      // property, which this inherits).
      svg.addEventListener("click", function (ev) {
        const pick = ev.target.closest && ev.target.closest("[data-board-role]");
        if (!pick) return;
        // Stop the click reaching the box's own enrol-on-click handler:
        // picking a board and binding a probe are different actions on the
        // same box, and one gesture must not do both.
        ev.stopPropagation();
        openBoardPicker(pick.getAttribute("data-board-role"));
      });
    }
    // Changing the board type changes which combinations exist, so the
    // second select is rebuilt rather than left showing the previous
    // board's revisions.
    const pickSelect = topoEl("role-board-select");
    if (pickSelect) {
      pickSelect.addEventListener("change", function () {
        renderRoleCombos(topoEl("role-board-dialog").dataset.role);
      });
    }
    const pickCancel = topoEl("role-board-cancel");
    if (pickCancel) pickCancel.addEventListener("click", closeBoardPicker);
    const pickBackdrop = topoEl("role-board-backdrop");
    if (pickBackdrop) pickBackdrop.addEventListener("click", closeBoardPicker);
    const pickSave = topoEl("role-board-save");
    if (pickSave) pickSave.addEventListener("click", saveRoleBoard);
    const pickRetract = topoEl("role-board-retract");
    if (pickRetract) pickRetract.addEventListener("click", retractRole);

    // The validate report carries the only control a leftover role has.
    const report = topoEl("topo-validate-report");
    if (report) {
      report.addEventListener("click", function (ev) {
        const btn = ev.target.closest("[data-unenroll-role]");
        if (btn) unenrollRole(btn.getAttribute("data-unenroll-role"));
      });
    }

    const rescan = topoEl("board-rescan");
    if (rescan) rescan.addEventListener("click", rescanBoards);

    loadBoardCatalog();
    loadProfiles();
    loadRolePickers();
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

  // --- Live Study tab ------------------------------------------------------
  //
  // One tab that runs a study, watches everything land as it arrives, and
  // opens a past one to read it back off disk. Both halves render through the
  // *same* cards, which is the whole reason the tab exists: a run watched live
  // and the same run reopened tomorrow must not look like two different
  // things.
  //
  // **The rings are the server's, not this file's.** `src/live_study.rs` holds
  // one subscription to embarch-core per study and everything that arrived on
  // it; this file connects to `/api/live/events`, is handed a snapshot of the
  // run so far, and appends from there. That is what makes reloading this page
  // mid-run replay the run rather than start at "now".

  var lsStudyId = null;
  var lsSource = null;
  var lsFeedFilter = "all";
  var lsAutoScroll = true;
  // The taps of whichever study is open, from its own record. Everything in
  // the Consoles, Trace and Data cards is decided from this — never from the
  // name or the content of a file.
  var lsTaps = [];
  var lsLiveSeries = {};
  var lsRecord = null;
  /// The terminal status this tab watched arrive, if it did.
  ///
  /// Load-bearing: `GET /study/{id}` is polled again after a run ends, and a
  /// status read *later* must never replace a terminal one read *earlier*.
  /// embarch-core's job registry is in memory and can still be reporting
  /// `running` for the moment between the last step landing and the job
  /// closing — rendering that over an observed `failed` would un-fail a study
  /// in front of the person who just watched it fail.
  var lsTerminal = null;

  function lsEl(id) {
    return document.getElementById(id);
  }

  /// An element with an optional class and id.
  ///
  /// Built with the DOM rather than an HTML string wherever the id is
  /// computed — `tests/element_ids.rs` reads this file's `id="…"` literals to
  /// find duplicates and dangling lookups, and a literal holding a
  /// concatenation (`id="' + name + '"`) reads to it as one id declared many
  /// times. That guard is worth more than the template.
  function lsMake(tag, cls, id) {
    var el = document.createElement(tag);
    if (cls) el.className = cls;
    if (id) el.id = id;
    return el;
  }

  /// One card, with a title and an optional note line under it.
  function lsCard(titleHtml, noteId) {
    var card = lsMake("div", "card");
    card.style.marginBottom = "16px";
    var title = lsMake("div", "card-title");
    title.innerHTML = titleHtml;
    card.appendChild(title);
    if (noteId) {
      var note = lsMake("p", "placeholder-note", noteId);
      note.style.marginTop = "0";
      card.appendChild(note);
    }
    return card;
  }


  function lsStatusBadge(status) {
    if (status === "completed") return "badge-success";
    if (status === "failed") return "badge-danger";
    if (status === "running" || status === "pending") return "badge-warning";
    // `interrupted` is its own thing and must never wear either neighbour's
    // colour: the study did not complete, and nothing said it failed
    // (embarch-core decision 69). It gets the warning shape and its own word.
    if (status === "interrupted") return "badge-warning";
    return "badge-neutral";
  }

  function lsWhen(ms) {
    if (ms == null) return '<span class="placeholder-note">—</span>';
    var d = new Date(ms);
    return '<span class="mono" title="' + escapeHtml(d.toISOString()) + '">' +
      escapeHtml(d.toLocaleString()) + "</span>";
  }

  // ---- the studies list ----------------------------------------------------

  async function lsLoadStudies() {
    var note = lsEl("ls-studies-note");
    var rows = lsEl("ls-studies-rows");
    note.textContent = "loading…";
    try {
      var resp = await fetch("/api/studies");
      var text = await resp.text();
      if (!resp.ok) {
        rows.innerHTML = "";
        note.textContent = resp.status + " " + text;
        return;
      }
      var data = JSON.parse(text);
      var studies = data.studies || [];
      note.innerHTML = studies.length
        ? escapeHtml(
            studies.length + " stud" + (studies.length === 1 ? "y" : "ies") +
            ", newest first" +
            (data.keep ? " — embarch-core keeps the last " + data.keep : " — retention is off, so this is every study on disk")
          )
        : "embarch-core has no study results on disk.";
      rows.innerHTML = studies
        .map(function (st) {
          var steps = st.steps
            ? escapeHtml(
                st.steps.total + " step" + (st.steps.total === 1 ? "" : "s") +
                (st.steps.failed ? " · " + st.steps.failed + " failed" : "") +
                (st.steps.timed_out ? " · " + st.steps.timed_out + " timed out" : "") +
                (st.steps.unknown ? " · " + st.steps.unknown + " unrecognised" : "")
              )
            // Absent, not zero. "Ran no steps" and "we could not read its
            // steps" are opposite facts and this row says which one it has.
            : '<span class="placeholder-note">not readable</span>';
          var taps = st.taps
            ? escapeHtml(String(st.taps.length))
            : '<span class="placeholder-note">—</span>';
          return (
            '<tr class="ls-study-row" data-study="' + escapeHtml(st.study_id) + '">' +
            '<td><div class="mono">' + escapeHtml(st.study_name || "(unnamed)") + "</div>" +
            '<div class="placeholder-note mono" style="font-size:11px;">' + escapeHtml(st.study_id) + "</div>" +
            (st.note ? '<div class="placeholder-note" style="color:var(--warning);">' + escapeHtml(st.note) + "</div>" : "") +
            "</td>" +
            '<td><span class="badge ' + lsStatusBadge(st.status) + '">' + escapeHtml(st.status) + "</span></td>" +
            "<td>" + steps + "</td>" +
            "<td>" + taps + "</td>" +
            "<td>" + lsWhen(st.started_utc_ms) + "</td>" +
            "</tr>"
          );
        })
        .join("");
    } catch (e) {
      note.textContent = String(e);
    }
  }

  // ---- the saved-study picker ---------------------------------------------

  async function lsLoadSavedStudies() {
    var select = lsEl("ls-study-picker");
    try {
      var resp = await fetch("/api/study-designer/studies");
      if (!resp.ok) {
        // 404 here is "no project open", which is a state, not a failure —
        // this tab can still open and read every past study without one.
        select.innerHTML = '<option value="">no project open — open one in the Study Designer</option>';
        return;
      }
      var data = await resp.json();
      // A bare array, not an object with a `studies` key — that route
      // predates this tab and its shape is its own. Both are accepted so a
      // reader here does not have to remember which it is.
      var studies = Array.isArray(data) ? data : data.studies || [];
      select.innerHTML = studies.length
        ? studies
            .map(function (st) {
              return '<option value="' + escapeHtml(st.slug) + '">' + escapeHtml(st.name || st.slug) + "</option>";
            })
            .join("")
        : '<option value="">this project has no saved study yet</option>';
    } catch (e) {
      select.innerHTML = '<option value="">' + escapeHtml(String(e)) + "</option>";
    }
  }

  async function lsRun() {
    var slug = lsEl("ls-study-picker").value;
    var err = lsEl("ls-run-error");
    err.style.display = "none";
    if (!slug) {
      err.textContent = "pick a saved study first";
      err.style.display = "block";
      return;
    }
    var btn = lsEl("ls-run");
    btn.disabled = true;
    try {
      var resp = await fetch("/api/live/run", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          slug: slug,
          allow_version_mismatch: lsEl("ls-allow-mismatch").checked,
        }),
      });
      var text = await resp.text();
      if (!resp.ok) {
        err.textContent = resp.status + " " + text;
        err.style.display = "block";
        return;
      }
      var data = JSON.parse(text);
      // A study that builds its own firmware has no id yet — the build has
      // to finish first. Same two shapes the Study Designer's own Run
      // handles; see `sdHandOffToLiveStudy`.
      if (data.build_id) {
        lsFollowBuild(data.build_id);
        return;
      }
      await lsOpenStudy(data.study_id, true);
      lsLoadStudies();
    } catch (e) {
      err.textContent = String(e);
      err.style.display = "block";
    } finally {
      btn.disabled = false;
    }
  }

  // ---- the build phase -----------------------------------------------------
  //
  // A build is a phase of the run, rendered as its own card above the status
  // one and torn down by nothing: it stays on screen after the study starts,
  // because "what was flashed" is part of reading the run that followed.
  //
  // The whole log is on disk regardless (the Debug tab's `builds` source).
  // This card is a live view, bounded like every other console here.
  var lsBuildStream = null;
  var LS_BUILD_MAX_LINES = 600;

  function lsBuildLine(kind, text) {
    var el = document.createElement("div");
    el.className = "log-line";
    if (kind === "stderr") el.classList.add("log-warn");
    if (kind === "info") el.classList.add("log-info");
    el.textContent = text;
    return el;
  }

  function lsFollowBuild(buildId) {
    var card = lsEl("ls-build-card");
    var console_ = lsEl("ls-build-console");
    card.style.display = "block";
    console_.innerHTML = "";
    lsEl("ls-build-error").style.display = "none";
    lsEl("ls-build-log-id").style.display = "none";
    lsEl("ls-build-badge").textContent = "building";
    lsEl("ls-build-badge").className = "badge badge-neutral";
    lsEl("ls-build-what").textContent = buildId;

    if (lsBuildStream) {
      lsBuildStream.close();
      lsBuildStream = null;
    }
    try {
      lsBuildStream = new EventSource("/api/build/events?id=" + encodeURIComponent(buildId));
    } catch (e) {
      lsEl("ls-build-error").textContent =
        "the build is running, but this browser could not open its stream: " + String(e) +
        ". Its log will still be in the Debug tab's builds source when it finishes.";
      lsEl("ls-build-error").style.display = "block";
      return;
    }
    lsBuildStream.addEventListener("build", function (evt) {
      var msg;
      try {
        msg = JSON.parse(evt.data);
      } catch (_) {
        return;
      }
      lsOnBuildEvent(msg);
    });
  }

  function lsOnBuildEvent(msg) {
    var console_ = lsEl("ls-build-console");
    var atBottom =
      console_.scrollHeight - console_.scrollTop - console_.clientHeight < 40;

    if (msg.kind === "line") {
      console_.appendChild(lsBuildLine(msg.stream, msg.text));
      while (console_.children.length > LS_BUILD_MAX_LINES) {
        console_.removeChild(console_.firstChild);
      }
    } else if (msg.kind === "phase") {
      lsEl("ls-build-badge").textContent =
        msg.phase === "submit" ? "submitting" : "building";
    } else if (msg.kind === "lagged") {
      // This browser fell behind our own broadcast. The card cannot claim to
      // be complete, and the whole log is on disk, so say both.
      console_.appendChild(
        lsBuildLine("info", "\u2026 " + msg.missed + " line(s) missed by this browser \u2014 the whole log is in the Debug tab's builds source")
      );
    } else if (msg.kind === "flashed") {
      lsEl("ls-build-what").textContent = msg.descriptor
        ? JSON.stringify(msg.descriptor)
        : msg.version;
      if (msg.log_id) {
        var idEl = lsEl("ls-build-log-id");
        idEl.textContent = msg.log_id;
        idEl.title = "this build's log, in the Debug tab's builds source";
        idEl.style.display = "";
      }
      console_.appendChild(lsBuildLine("info", "flashed " + msg.version));
    } else if (msg.kind === "started") {
      lsEl("ls-build-badge").textContent = "flashed";
      lsEl("ls-build-badge").className = "badge badge-ok";
      if (lsBuildStream) {
        lsBuildStream.close();
        lsBuildStream = null;
      }
      lsOpenStudy(msg.study_id, true);
      lsLoadStudies();
    } else if (msg.kind === "failed") {
      lsEl("ls-build-badge").textContent =
        msg.phase === "submit" ? "not submitted" : "build failed";
      lsEl("ls-build-badge").className = "badge badge-error";
      var err = lsEl("ls-build-error");
      err.textContent = msg.error || "the build failed";
      err.style.display = "block";
      if (lsBuildStream) {
        lsBuildStream.close();
        lsBuildStream = null;
      }
    }

    if (atBottom) console_.scrollTop = console_.scrollHeight;
  }

  // ---- opening one study ---------------------------------------------------

  /// Opens `studyId` in every card below, live where it is still running and
  /// off disk where it is not.
  ///
  /// `live` is a hint, not the decision: a study the record says is running
  /// gets a live session whatever the caller thought. What the caller knows
  /// that the record does not is the case of a study *just now* submitted,
  /// whose job embarch-core may not have moved off `pending` yet.
  async function lsOpenStudy(studyId, live) {
    if (!studyId) return;
    lsStudyId = studyId;
    lsLiveSeries = {};
    lsTerminal = null;
    lsEl("ls-body").style.display = "block";
    lsEl("ls-status-id").textContent = studyId;
    lsEl("ls-steps-rows").innerHTML = "";
    lsEl("ls-feed").innerHTML = "";
    lsEl("ls-consoles").innerHTML = "";
    lsEl("ls-data").innerHTML = "";
    lsEl("ls-status-lagged").style.display = "none";
    lsEl("ls-record-note").style.display = "none";
    tcHideDetail();
    lsDetach();

    await lsLoadRecord(studyId, true);
    var job = (lsRecord && lsRecord.job) || null;
    var running = job && (job.status === "running" || job.status === "pending");
    if (live || running) lsAttach(studyId);
  }

  /// Reads the study's own record and renders it.
  ///
  /// `rebuild` builds the console, trace and data cards from scratch, which is
  /// right when opening a study and wrong when a run has just ended: tearing
  /// the cards down there would throw away a console and a plot this tab
  /// watched arrive, in exchange for whatever the disk read happens to
  /// return. So a post-run read *refreshes* the same cards instead, and each
  /// capture replaces its card's contents **only if its own read succeeded**.
  async function lsLoadRecord(studyId, rebuild) {
    lsRecord = null;
    lsTaps = [];
    try {
      var resp = await fetch("/api/studies/" + encodeURIComponent(studyId));
      var text = await resp.text();
      if (!resp.ok) {
        lsNote("ls-record-note", resp.status + " " + text);
        return;
      }
      lsRecord = JSON.parse(text);
    } catch (e) {
      lsNote("ls-record-note", String(e));
      return;
    }

    lsTaps = lsRecord.taps || [];
    // Each part says for itself whether it could be read. A study whose
    // events.json this embarch-core cannot parse still has a readable stream
    // index, and hiding the taps because the steps failed would be losing the
    // half that worked.
    lsEl("ls-steps-note").innerHTML = lsRecord.steps_note
      ? '<span style="color:var(--warning);">' + escapeHtml(lsRecord.steps_note) + "</span>"
      : "Rows fill in as each step reports, not once the study ends.";
    if (lsRecord.steps && lsRecord.steps.steps) {
      lsRenderSteps(
        lsRecord.steps.steps.map(function (st) {
          return {
            index: st.index,
            step_name: st.step_name,
            outcome: st.outcome,
            reason: st.reason,
          };
        })
      );
      if (lsRecord.steps.study_name) {
        lsEl("ls-status-name").textContent = lsRecord.steps.study_name;
      }
    }
    if (lsRecord.job) {
      lsRenderStatus({
        status: lsRecord.job.status,
        reason: lsRecord.job.reason,
        current_step: lsRecord.job.current_step,
        total_steps: lsRecord.job.total_steps,
        mode: null,
        lagged: 0,
      });
      renderRunStreams(lsEl("ls-streams"), studyId, (lsRecord.job || {}).streams);
    } else if (lsRecord.taps_note == null) {
      // embarch-core 404s a study id its registry has forgotten, which after
      // a restart is every study that ever ran. That is the ordinary case for
      // a post-hoc read, not an error — the listing is where such a study's
      // status comes from, and it is already on screen.
      lsEl("ls-status-mode").textContent = "read from disk";
    }
    renderProvenance(lsEl("ls-provenance"), lsRecord.provenance);
    if (lsRecord.taps_note) lsNote("ls-record-note", lsRecord.taps_note);

    lsRenderTraceTaps();
    // The Time chart reads the whole study rather than one tap, so it is
    // loaded here rather than off the tap picker. Awaited **after** the
    // trace picker is populated and before the per-tap cards, so a slow
    // multi-megabyte capture does not hold up the cards that are already
    // drawable.
    await tcLoad();
    if (rebuild) {
      await lsRenderConsoles();
      await lsRenderData();
    } else {
      await lsRefreshCaptures();
    }
  }

  /// Re-reads every capture off disk into the cards already on screen.
  ///
  /// Each read replaces its own card's contents only on success, so a tap
  /// whose file embarch-core will not serve keeps what the live feed put
  /// there — which is less than the file, and is the only thing there is.
  async function lsRefreshCaptures() {
    var text = lsTextTaps();
    for (var i = 0; i < text.length; i++) {
      await lsLoadConsoleFromDisk(text[i].name);
    }
    var data = lsDataTaps();
    for (var j = 0; j < data.length; j++) {
      if (data[j].encoding === "Raw") await lsLoadHex(data[j].name);
      else await lsLoadRows(data[j].name, 0);
    }
  }

  function lsNote(id, text) {
    var el = lsEl(id);
    if (!text) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.textContent = text;
  }

  // ---- the live stream -----------------------------------------------------

  function lsDetach() {
    if (lsSource) {
      lsSource.close();
      lsSource = null;
    }
  }

  function lsAttach(studyId) {
    lsDetach();
    lsSource = new EventSource("/api/live/events?study=" + encodeURIComponent(studyId));
    lsSource.addEventListener("live", function (ev) {
      var frame;
      try {
        frame = JSON.parse(ev.data);
      } catch (e) {
        // A malformed frame must not kill the stream.
        return;
      }
      lsApplyFrame(frame);
    });
  }

  function lsApplyFrame(frame) {
    if (frame.kind === "snapshot") return lsApplySnapshot(frame);
    if (frame.kind === "time_chart") return tcApplyLive(frame);
    if (frame.kind === "console") return lsApplyConsole(frame);
    if (frame.kind === "samples") return lsApplySamples(frame);
    if (frame.kind === "step") {
      lsAppendStep(frame.step);
      lsRenderStatus(frame.status);
      lsAppendFeed(frame.row);
      return;
    }
    if (frame.kind === "status") {
      lsRenderStatus(frame.status);
      if (frame.row) lsAppendFeed(frame.row);
      if (frame.provenance) renderProvenance(lsEl("ls-provenance"), frame.provenance);
      if (frame.streams) renderRunStreams(lsEl("ls-streams"), lsStudyId, frame.streams);
      if (frame.status && frame.status.finished) lsOnFinished();
      return;
    }
    if (frame.kind === "gatt" || frame.kind === "event") {
      if (frame.row) lsAppendFeed(frame.row);
      return;
    }
    if (frame.kind === "browser_lagged") {
      // This browser fell behind embarch-ui's own broadcast, which is a
      // different fact from embarch-core's `lagged` and is not folded into
      // it. The server's rings are intact, so reloading this page replays
      // everything — and that is what the note says to do.
      lsNote(
        "ls-record-note",
        "this browser fell behind embarch-ui's own feed and missed " + frame.missed +
          " frame(s) — the server still has the whole run, so reload this page to catch up"
      );
    }
  }

  function lsApplySnapshot(frame) {
    lsRenderStatus(frame.status);
    lsEl("ls-steps-rows").innerHTML = "";
    (frame.steps || []).forEach(lsAppendStep);
    lsEl("ls-feed").innerHTML = "";
    (frame.feed || []).forEach(lsAppendFeed);
    lsRenderFeedNote(frame.feed_dropped, frame.feed_total);
    (frame.consoles || []).forEach(function (c) {
      lsApplyConsole({ tap: c.tap, lines: c.lines, partial: c.partial, dropped: c.dropped, total: c.total, replace: true });
    });
    (frame.series || []).forEach(function (sr) {
      lsLiveSeries[sr.tap] = { points: sr.points || [], stride: sr.stride || 1, total: sr.total || 0 };
      lsDrawLiveSeries(sr.tap);
    });
    if (frame.provenance) renderProvenance(lsEl("ls-provenance"), frame.provenance);
    if (frame.streams) renderRunStreams(lsEl("ls-streams"), lsStudyId, frame.streams);
    // The chart the run has built so far, replayed whole. This is what makes
    // opening or reloading the tab mid-run give the same picture as watching
    // from the start — the marks are placed against the axis as it stands
    // *now*, which is the only version of it that is true.
    if (frame.time_chart) tcApplyLive(frame.time_chart);
  }

  function lsIsTerminal(status) {
    return status === "completed" || status === "failed" || status === "interrupted";
  }

  function lsRenderStatus(status) {
    if (!status) return;
    if (lsIsTerminal(status.status)) {
      lsTerminal = status.status;
    } else if (lsTerminal) {
      // A non-terminal reading arriving after a terminal one is stale, not
      // news — see `lsTerminal`. The rest of this status (its mode, its
      // lagged count) is still worth taking, so only the verdict is kept.
      status = Object.assign({}, status, { status: lsTerminal });
    }
    var badge = lsEl("ls-status-badge");
    var text = status.status || "pending";
    if (text === "running") text += sdRunningStepLabel(status.current_step, status.total_steps);
    badge.className = "badge " + lsStatusBadge(status.status);
    badge.textContent = text;
    if (status.study_name) lsEl("ls-status-name").textContent = status.study_name;
    lsNote("ls-status-reason", status.reason || "");
    // Which transport is in force, said rather than implied: a feed being
    // polled is a feed with a cadence, not a live one.
    lsEl("ls-status-mode").textContent = status.mode
      ? status.mode === "polling"
        ? "polling embarch-core — the live stream is not in use"
        : "live"
      : "";
    if (status.lagged) {
      lsNote(
        "ls-status-lagged",
        "embarch-core dropped " + status.lagged +
          " event(s) from this feed — the study is unaffected and its record on disk is complete. " +
          "Reopen this study once it ends to read the complete record."
      );
    }
    if (status.record_note) lsNote("ls-record-note", status.record_note);
  }

  async function lsOnFinished() {
    // The run is over, so the file is the better source than the feed: the
    // plots re-render from the rendered CSV, the consoles from the captured
    // text, and the trace becomes drawable for the first time.
    var id = lsStudyId;
    await lsLoadRecord(id, false);
    lsLoadStudies();
  }

  // ---- steps ---------------------------------------------------------------

  function lsRenderSteps(steps) {
    lsEl("ls-steps-rows").innerHTML = "";
    (steps || []).forEach(lsAppendStep);
  }

  function lsAppendStep(step) {
    if (!step) return;
    var rows = lsEl("ls-steps-rows");
    var index = step.index == null ? rows.children.length : step.index;
    var tr = document.createElement("tr");
    tr.innerHTML =
      "<td>" + (index + 1) + "</td>" +
      '<td class="mono">' + escapeHtml(step.step_name || "") + "</td>" +
      "<td>" + outcomeBadge(step.outcome, step.reason) + "</td>" +
      "<td>" + stepDetail(step) + "</td>";
    rows.appendChild(tr);
  }

  // ---- the event feed ------------------------------------------------------

  function lsRenderFeedNote(dropped, total) {
    var note = lsEl("ls-feed-note");
    if (!dropped) {
      note.textContent = total ? total + " event(s)" : "";
      return;
    }
    // A capped ring says it is capped. Never a silent drop.
    note.innerHTML =
      '<span style="color:var(--warning);">showing the last ' + (total - dropped) +
      " of " + total + " events — the feed is capped, and embarch-core's own record on disk is not</span>";
  }

  function lsAppendFeed(row) {
    if (!row) return;
    var feed = lsEl("ls-feed");
    var div = document.createElement("div");
    div.className = "ls-feed-row ls-feed-" + row.kind;
    div.setAttribute("data-feed-kind", row.kind);
    div.innerHTML =
      '<span class="ls-feed-kind">' + escapeHtml(row.kind) + "</span>" +
      '<span class="ls-feed-text">' + escapeHtml(row.text) + "</span>";
    if (lsFeedFilter !== "all" && row.kind !== lsFeedFilter) div.style.display = "none";
    feed.appendChild(div);
    if (lsAutoScroll) feed.scrollTop = feed.scrollHeight;
  }

  function lsApplyFeedFilter() {
    var rows = lsEl("ls-feed").children;
    for (var i = 0; i < rows.length; i++) {
      var kind = rows[i].getAttribute("data-feed-kind");
      rows[i].style.display = lsFeedFilter === "all" || kind === lsFeedFilter ? "" : "none";
    }
  }

  // ---- consoles ------------------------------------------------------------
  //
  // One card per `Text`-encoded tap. That is the whole rule — `dev-bench` is
  // the reserved one every study carries, and a DUT shell is a `Text` tap the
  // study declared on a notify characteristic. A study that declares neither
  // gets no console card at all rather than an empty one.

  function lsTextTaps() {
    return lsTaps.filter(function (t) {
      return t.is_text;
    });
  }

  function lsConsoleId(tap) {
    return "ls-console-" + tap.replace(/[^A-Za-z0-9_-]/g, "_");
  }

  async function lsRenderConsoles() {
    var host = lsEl("ls-consoles");
    host.innerHTML = "";
    var taps = lsTextTaps();
    if (!taps.length) return;
    taps.forEach(function (t) {
      host.appendChild(lsConsoleCard(t.name));
    });
    // Off disk for a study that has finished. A live one overwrites this the
    // moment its snapshot arrives.
    for (var i = 0; i < taps.length; i++) {
      await lsLoadConsoleFromDisk(taps[i].name);
    }
  }

  function lsConsoleCard(tap) {
    var id = lsConsoleId(tap);
    var card = lsCard(
      'Console \u2014 <span class="mono">' + escapeHtml(tap) + "</span>",
      id + "-note"
    );
    card.appendChild(lsMake("div", "ls-console", id));
    return card;
  }

  async function lsLoadConsoleFromDisk(tap) {
    var el = lsEl(lsConsoleId(tap));
    if (!el) return;
    try {
      var resp = await fetch(
        "/api/studies/" + encodeURIComponent(lsStudyId) + "/stream/" +
          encodeURIComponent(tap) + "/text?limit=2000"
      );
      if (!resp.ok) return;
      var data = await resp.json();
      el.innerHTML = (data.lines || [])
        .map(function (line) {
          return '<div class="ls-console-line">' + escapeHtml(line) + "</div>";
        })
        .join("") +
        (data.partial
          ? '<div class="ls-console-line ls-console-partial">' + escapeHtml(data.partial) + "</div>"
          : "");
      var note = lsEl(lsConsoleId(tap) + "-note");
      if (note) {
        note.textContent =
          data.total + " line(s), " + data.bytes + " bytes captured" +
          (data.total > (data.lines || []).length
            ? " — showing the first " + (data.lines || []).length
            : "") +
          (data.partial ? " · the last line never got a newline and is shown as partial" : "");
      }
      el.scrollTop = el.scrollHeight;
    } catch (e) {
      /* a console that will not load is not worth failing the tab over */
    }
  }

  function lsApplyConsole(frame) {
    var id = lsConsoleId(frame.tap);
    var el = lsEl(id);
    if (!el) {
      // A tap the record did not list — a study whose stream index could not
      // be read, most likely. Make a card for it rather than dropping the
      // console on the floor.
      lsEl("ls-consoles").appendChild(lsConsoleCard(frame.tap));
      el = lsEl(id);
    }
    if (frame.replace) el.innerHTML = "";
    // The partial line is redrawn every frame — it is not a line yet, and it
    // grows until a newline arrives.
    var stale = el.querySelector(".ls-console-partial");
    if (stale) stale.remove();
    (frame.lines || []).forEach(function (line) {
      var div = document.createElement("div");
      div.className = "ls-console-line" + (line.truncated ? " ls-console-cut" : "");
      div.textContent = line.text + (line.truncated ? "  … (line cut at 8 KB)" : "");
      el.appendChild(div);
    });
    if (frame.partial) {
      var p = document.createElement("div");
      p.className = "ls-console-line ls-console-partial";
      // Marked as partial, never padded into a line this tap did not send.
      p.textContent = frame.partial;
      el.appendChild(p);
    }
    var note = lsEl(id + "-note");
    if (note) {
      note.textContent =
        frame.total + " line(s)" +
        (frame.dropped
          ? " — showing the last " + (frame.total - frame.dropped) + ", the console ring is capped"
          : "") +
        (frame.partial ? " · the last line has not ended yet" : "");
    }
    el.scrollTop = el.scrollHeight;
  }

  // ---- the trace card ------------------------------------------------------

  function lsRenderTraceTaps() {
    var input = trEl("trace-study");
    if (!input) return;
    input.value = lsStudyId || "";
    var select = trEl("trace-tap");
    var traces = lsTaps.filter(function (t) {
      return t.is_outpost_trace;
    });
    if (!traces.length) {
      select.disabled = true;
      select.innerHTML = '<option value="">this study declared no outpost trace</option>';
      trEl("trace-body").style.display = "none";
      trEl("trace-refusal").style.display = "none";
      traceShowError("");
      return;
    }
    select.disabled = false;
    select.innerHTML = traces
      .map(function (t) {
        return '<option value="' + escapeHtml(t.name) + '">' + escapeHtml(t.name) +
          (t.named ? "" : " — unnamed") + "</option>";
      })
      .join("");
    traceLoadView();
  }

  // ---- data cards ----------------------------------------------------------
  //
  // One card per tap that is not a console and not the trace: the GATT
  // transcript, sample and struct tables, and a `Raw` tap's own bytes. **This
  // file parses no CSV** — the rows, the column names and the plot's bins all
  // arrive decoded (`src/studies_api.rs`), the same rule the chart above holds.

  function lsDataId(tap) {
    return "ls-data-" + tap.replace(/[^A-Za-z0-9_-]/g, "_");
  }

  function lsDataTaps() {
    return lsTaps.filter(function (t) {
      return !t.is_text && !t.is_outpost_trace;
    });
  }

  async function lsRenderData() {
    var host = lsEl("ls-data");
    host.innerHTML = "";
    var taps = lsDataTaps();
    if (!taps.length) return;
    taps.forEach(function (t) {
      host.appendChild(lsDataCard(t));
    });
    for (var i = 0; i < taps.length; i++) {
      if (taps[i].encoding === "Raw") await lsLoadHex(taps[i].name);
      else await lsLoadRows(taps[i].name, 0);
    }
  }

  function lsDataCard(tap) {
    var id = lsDataId(tap.name);
    var card = lsMake("div", "card");
    card.style.marginBottom = "16px";

    var bar = lsMake("div", "sd-toolbar");
    var title = lsMake("div", "card-title");
    title.style.margin = "0";
    title.innerHTML =
      escapeHtml(tap.name) +
      ' <span class="placeholder-note mono" style="font-weight:normal;">' +
      escapeHtml(lsEncodingLabel(tap.encoding)) + "</span>";
    bar.appendChild(title);
    var actions = lsMake("div", "sd-toolbar-actions");
    var dl = lsMake("a", "btn");
    dl.setAttribute("download", "");
    dl.href =
      "/api/studies/" + encodeURIComponent(lsStudyId) + "/stream/" +
      encodeURIComponent(tap.name) + "/download";
    dl.textContent = "Download";
    actions.appendChild(dl);
    bar.appendChild(actions);
    card.appendChild(bar);

    if (tap.note) {
      var warn = lsMake("p", "placeholder-note");
      warn.style.color = "var(--warning)";
      warn.textContent = tap.note;
      card.appendChild(warn);
    }
    var note = lsMake("p", "placeholder-note", id + "-note");
    note.style.margin = "8px 0";
    card.appendChild(note);

    if (tap.encoding === "Raw") {
      card.appendChild(lsMake("div", "ls-console", id + "-hex"));
      return card;
    }

    card.appendChild(lsMake("div", null, id + "-plot"));
    var scroll = lsMake("div", "table-scroll");
    scroll.style.maxHeight = "360px";
    scroll.style.marginTop = "10px";
    var table = lsMake("table", "data-table");
    table.appendChild(lsMake("thead", null, id + "-head"));
    table.appendChild(lsMake("tbody", null, id + "-rows"));
    scroll.appendChild(table);
    card.appendChild(scroll);

    var more = lsMake("div", "sd-row-actions");
    more.style.marginTop = "10px";
    var btn = lsMake("button", "btn btn-tiny");
    btn.setAttribute("data-rows-more", tap.name);
    btn.textContent = "Show more rows";
    more.appendChild(btn);
    card.appendChild(more);
    return card;
  }

  function lsEncodingLabel(encoding) {
    if (encoding == null) return "";
    if (typeof encoding === "string") return encoding;
    // `Samples { layout, unit, … }` and `Struct { decoder }` are tagged
    // objects. The tag is the part a person reads; the body is the study's
    // own declaration and is on the Study Designer's Streams card.
    var keys = Object.keys(encoding);
    return keys.length ? keys[0] : "";
  }

  async function lsLoadHex(tap) {
    var el = lsEl(lsDataId(tap) + "-hex");
    var note = lsEl(lsDataId(tap) + "-note");
    if (!el) return;
    try {
      var resp = await fetch(
        "/api/studies/" + encodeURIComponent(lsStudyId) + "/stream/" +
          encodeURIComponent(tap) + "/head"
      );
      if (!resp.ok) return;
      var data = await resp.json();
      el.innerHTML = (data.lines || [])
        .map(function (l) {
          return '<div class="ls-console-line">' + escapeHtml(l) + "</div>";
        })
        .join("");
      // A `Raw` tap renders nothing because nobody declared anything to
      // render it as (embarch-study-designer decision 39). The hex head is
      // the honest view; the download is the whole capture.
      note.textContent =
        data.total_bytes + " bytes captured, nothing declared to decode them as — showing the first " +
        data.shown_bytes;
    } catch (e) {
      /* the bytes are on embarch-core's disk either way */
    }
  }

  async function lsLoadRows(tap, from) {
    var id = lsDataId(tap);
    var head = lsEl(id + "-head");
    var rows = lsEl(id + "-rows");
    var note = lsEl(id + "-note");
    if (!head) return;
    try {
      var resp = await fetch(
        "/api/studies/" + encodeURIComponent(lsStudyId) + "/stream/" +
          encodeURIComponent(tap) + "/rows?from=" + from + "&limit=100"
      );
      var text = await resp.text();
      if (!resp.ok) {
        note.innerHTML = '<span style="color:var(--warning);">' + escapeHtml(resp.status + " " + text) + "</span>";
        return;
      }
      var data = JSON.parse(text);
      if (from === 0) {
        head.innerHTML =
          "<tr>" + (data.columns || []).map(function (c) { return "<th>" + escapeHtml(c) + "</th>"; }).join("") + "</tr>";
        rows.innerHTML = "";
      }
      (data.rows || []).forEach(function (row) {
        var tr = document.createElement("tr");
        tr.innerHTML = row
          .map(function (cell) {
            return '<td class="mono">' + escapeHtml(cell) + "</td>";
          })
          .join("");
        rows.appendChild(tr);
      });
      var shown = rows.children.length;
      note.textContent = data.total
        ? "showing " + shown + " of " + data.total + " row(s)"
        : "this tap captured no rows";
      if (from === 0 && (data.numeric_columns || []).length) {
        lsLoadSeries(tap, data.numeric_columns, data.numeric_columns[0], data.time_column);
      }
    } catch (e) {
      note.textContent = String(e);
    }
  }

  // ---- plots ---------------------------------------------------------------

  async function lsLoadSeries(tap, columns, column, timeColumn) {
    var host = lsEl(lsDataId(tap) + "-plot");
    if (!host) return;
    try {
      var resp = await fetch(
        "/api/studies/" + encodeURIComponent(lsStudyId) + "/stream/" +
          encodeURIComponent(tap) + "/series?width=600&column=" + encodeURIComponent(column)
      );
      if (!resp.ok) return;
      var data = await resp.json();
      host.innerHTML =
        '<div class="sd-toolbar" style="margin-bottom:6px;">' +
        '<label class="sd-field" style="min-width:180px;"><span>Plot column</span>' +
        '<select class="sd-input mono" data-series-tap="' + escapeHtml(tap) + '">' +
        columns
          .map(function (c) {
            return '<option value="' + escapeHtml(c) + '"' + (c === column ? " selected" : "") + ">" + escapeHtml(c) + "</option>";
          })
          .join("") +
        "</select></label>" +
        '<span class="placeholder-note" style="margin:0;">' +
        escapeHtml(
          data.points + " point(s)" +
          (timeColumn ? " against " + timeColumn : " against row index — this tap carries no arrival stamp") +
          (data.unparsed ? " · " + data.unparsed + " value(s) this build could not read as a number" : "")
        ) +
        "</span></div>" +
        lsPlotSvg(data.bins || []);
    } catch (e) {
      /* a plot that will not draw leaves the table, which is the data */
    }
  }

  /// One min/max band per bin. **Min and max, never an average** — an average
  /// hides the spike that is usually the reason somebody is looking.
  function lsPlotSvg(bins, preview) {
    if (!bins.length) return '<p class="placeholder-note">nothing to plot yet</p>';
    var w = 600;
    var h = 160;
    var lo = Infinity;
    var hi = -Infinity;
    bins.forEach(function (b) {
      if (b.min < lo) lo = b.min;
      if (b.max > hi) hi = b.max;
    });
    if (!(hi > lo)) {
      hi = lo + 1;
      lo = lo - 1;
    }
    var x0 = bins[0].x;
    var x1 = bins[bins.length - 1].x;
    var xspan = x1 - x0 || 1;
    var path = bins
      .map(function (b) {
        var x = ((b.x - x0) / xspan) * (w - 2) + 1;
        var yTop = h - ((b.max - lo) / (hi - lo)) * (h - 2) - 1;
        var yBot = h - ((b.min - lo) / (hi - lo)) * (h - 2) - 1;
        return "M" + x.toFixed(2) + " " + yTop.toFixed(2) + "V" + yBot.toFixed(2);
      })
      .join("");
    return (
      '<svg class="ls-plot" viewBox="0 0 ' + w + " " + h + '" preserveAspectRatio="none" style="width:100%; height:' + h + 'px;">' +
      '<path d="' + path + '" stroke="var(--accent)" stroke-width="1.2" fill="none" />' +
      "</svg>" +
      '<div class="placeholder-note" style="display:flex; justify-content:space-between;">' +
      "<span>" + escapeHtml(lo.toPrecision(4)) + "</span>" +
      (preview ? '<span style="color:var(--warning);">live preview — redrawn from the capture when the run ends</span>' : "") +
      "<span>" + escapeHtml(hi.toPrecision(4)) + "</span></div>"
    );
  }

  function lsApplySamples(frame) {
    var series = lsLiveSeries[frame.tap];
    if (!series) {
      series = { points: [], stride: 1, total: 0 };
      lsLiveSeries[frame.tap] = series;
    }
    (frame.points || []).forEach(function (p) {
      series.points.push(p);
    });
    series.stride = frame.stride || 1;
    series.total = frame.total || series.total;
    lsDrawLiveSeries(frame.tap);
    if (frame.row) lsAppendFeed(frame.row);
  }

  /// A live plot, drawn from what has arrived so far and **labelled a
  /// preview**. It is redrawn from the rendered capture the moment the run
  /// ends (`lsOnFinished`), because the file is complete and this is not.
  function lsDrawLiveSeries(tap) {
    var series = lsLiveSeries[tap];
    if (!series || !series.points.length) return;
    var id = lsDataId(tap);
    var host = lsEl(id + "-plot");
    if (!host) {
      // A sample tap whose data card does not exist yet — a run started
      // before the record was read. The console/data cards are rebuilt when
      // the record arrives, and this plot comes with them.
      return;
    }
    var bins = series.points.map(function (p) {
      return { x: p[0], min: p[1], max: p[1], count: 1 };
    });
    host.innerHTML =
      '<p class="placeholder-note" style="margin:0 0 6px;">' +
      escapeHtml(
        series.total + " sample(s)" +
        (series.stride > 1
          ? " — plotting one in " + series.stride + ", this series is decimated"
          : "")
      ) +
      "</p>" +
      lsPlotSvg(bins, true);
  }

  // ---- wiring --------------------------------------------------------------

  function initLiveStudyTab() {
    if (!lsEl("ls-studies-rows")) return;

    lsEl("ls-studies-refresh").addEventListener("click", lsLoadStudies);
    lsEl("ls-run").addEventListener("click", lsRun);
    lsEl("ls-open").addEventListener("click", function () {
      var id = lsEl("ls-open-id").value.trim();
      if (id) lsOpenStudy(id, false);
    });
    lsEl("ls-open-id").addEventListener("keydown", function (ev) {
      if (ev.key !== "Enter") return;
      var id = lsEl("ls-open-id").value.trim();
      if (id) lsOpenStudy(id, false);
    });
    lsEl("ls-studies-rows").addEventListener("click", function (ev) {
      var row = ev.target.closest("[data-study]");
      if (!row) return;
      lsOpenStudy(row.getAttribute("data-study"), false);
    });
    lsEl("ls-feed-filters").addEventListener("click", function (ev) {
      var chip = ev.target.closest("[data-feed-kind]");
      if (!chip) return;
      Array.prototype.forEach.call(lsEl("ls-feed-filters").children, function (c) {
        c.classList.remove("active-filter");
      });
      chip.classList.add("active-filter");
      lsFeedFilter = chip.getAttribute("data-feed-kind");
      lsApplyFeedFilter();
    });
    // Auto-scroll that pauses when you scroll up: a feed that yanks itself
    // back to the bottom while somebody is reading it is unreadable.
    lsEl("ls-feed").addEventListener("scroll", function () {
      var el = lsEl("ls-feed");
      lsAutoScroll = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    });
    lsEl("ls-data").addEventListener("click", function (ev) {
      var more = ev.target.closest("[data-rows-more]");
      if (!more) return;
      var tap = more.getAttribute("data-rows-more");
      var rows = lsEl(lsDataId(tap) + "-rows");
      lsLoadRows(tap, rows ? rows.children.length : 0);
    });
    lsEl("ls-data").addEventListener("change", function (ev) {
      var select = ev.target.closest("[data-series-tap]");
      if (!select) return;
      var tap = select.getAttribute("data-series-tap");
      var columns = Array.prototype.map.call(select.options, function (o) {
        return o.value;
      });
      lsLoadSeries(tap, columns, select.value, null);
    });

    lsLoadStudies();
    lsLoadSavedStudies();
  }

  // --- Trace view (decision 10, second half) ------------------------------
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
    traceForgetBins();
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
    // (`embarch-outpost` decision 19). Rendered against the
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

    // **"every row in the capture" is a claim, and it has to be earned.** Two
    // separate counts can falsify it: this view's own row cap (`view.row_cap`,
    // `MAX_ROWS` in `src/trace.rs` — served rather than restated here, per
    // `embarch-ui/spec.md`'s Invariants), and rows the decoder refused
    // outright — a truncated final write, or a row whose frame index is not a
    // number. Only the cap was ever reported, so a capture cut off mid-write
    // read as complete, which is exactly the hole `embarch-ui/spec.md`'s
    // "unreadable is rendered as unreadable" forbids. Both are stated when
    // non-zero; the sentence is unchanged when neither is.
    var recordsNotes = [];
    if (view.rows_dropped_by_cap > 0) {
      // No literal fallback number: this view and `embarch-core` ship as one
      // binary, so `row_cap` is always present in practice, but a defaulted
      // number here would be exactly the restated-limit defect this field
      // exists to remove — a copy that can silently disagree with the real
      // cap is no better for being spelled in JS instead of committed twice.
      // Missing means unknown, said as unknown, never guessed.
      var capNote = typeof view.row_cap === "number"
        ? "this view caps at " + view.row_cap.toLocaleString()
        : "this view has a row cap (unreported by this server)";
      recordsNotes.push(view.rows_dropped_by_cap + " more not read — " + capNote);
    }
    if (view.rows_unparsed > 0) {
      recordsNotes.push(view.rows_unparsed + " row(s) unreadable — truncated or malformed, " +
        "refused rather than guessed at");
    }
    trEl("trace-stats").innerHTML =
      statCard("Records", String(view.rows),
        recordsNotes.length ? recordsNotes.join(" · ") : "every row in the capture",
        view.rows_unparsed > 0 ? "var(--warning)" : null) +
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
    // A dropped stale prefix is reported rather than shown as a shorter
    // capture. It is the one note here that describes records the reader
    // cannot see: `rows` counts what was kept, so without this line the
    // capture would simply be N records shorter than the file, with the
    // microsecond axis it only has *because* they went.
    var staleNote = view.stale_prefix_rows > 0
      ? " · the first " + view.stale_prefix_rows + " record(s) were dropped: their DUT clock sat " +
        (view.stale_prefix_step_us / 1000000).toFixed(3) + " s from the capture that follows them, " +
        "which is longer than that capture lasted — bytes already inside the USB-UART bridge when " +
        "embarch-core opened the port, from before the DUT reset. Dropping them is what keeps the " +
        "microsecond axis for everything after"
      : "";
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
      staleNote + dutNote + clockNote;

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

  // ---- shared chart geometry ------------------------------------------------
  //
  // **Two charts, one pixel map.** The Time chart and the Trace chart draw the
  // same axis over the same capture, stacked one above the other, and a reader
  // reads across them. Two independently-derived pixel maps agree in the
  // middle and disagree at the edges by a fraction of a column — which is a
  // mark drawn one pixel away from the span it happened inside, and the one
  // error this pair of charts exists to make impossible. So there is one
  // clamp, one set of zoom floors and one pixel map, and both charts go
  // through them.
  //
  // Everything here takes an **axis** — anything carrying `t_from`, `t_to` and
  // `unit` — rather than a view, so a `TraceView` and a `TimeChartView.axis`
  // are both valid arguments without either learning about the other.

  /// The right-hand inset of a plot, shared so both charts end at the same x.
  var CHART_PAD_RIGHT = 14;

  function chartFullWin(axis) {
    return { from: axis.t_from, to: Math.max(axis.t_from + 1, axis.t_to) };
  }

  /// The finest window a reader may zoom to. A floor is needed because the
  /// axis is integers: a window narrower than a few units would put several
  /// pixel columns inside one unit, and the binning would draw a span's
  /// *rounding* rather than its extent. The floors differ per clock because
  /// the units do — 40 µs of the DUT's counter, 4 ms of the host's, 4 frames
  /// when there is no time base at all.
  function chartMinWin(unit) {
    if (unit === "us") return 40;
    return 4;
  }

  /// Clamps a proposed window into the capture. Zoom never goes wider than the
  /// whole capture and pan never leaves it, so there is no way to end up
  /// looking at empty axis and wondering whether the trace stopped.
  function chartClampWin(axis, win) {
    var full = chartFullWin(axis);
    var extent = full.to - full.from;
    var w = Math.min(extent, Math.max(chartMinWin(axis.unit), win.to - win.from));
    var from = Math.min(Math.max(win.from, full.from), full.to - w);
    // **Rounded to whole units, and that is load-bearing rather than tidy.**
    // The axis is integers (`chartMinWin` says why), but zooming at the
    // pointer computes an anchor from a pixel fraction, so every wheel notch
    // produced a fractional window. That was invisible while aggregation
    // happened in the browser and float bounds only shifted a rect by a
    // sub-pixel; now the window is a request the server answers, and
    // `?from=1234.56` is not a narrower window, it is a malformed query.
    // Rounding the width first and the origin second keeps the zoom floor
    // exact.
    w = Math.round(w);
    return { from: Math.round(from), to: Math.round(from) + w };
  }

  /// Axis units per CSS pixel of plot, for a drag.
  function chartUnitsPerPx(widthPx, gutter, win) {
    var plotW = Math.max(1, widthPx - CHART_PAD_RIGHT - gutter);
    return (win.to - win.from) / plotW;
  }

  /// The axis value under a pointer event. `null` when the pointer is left of
  /// the plot (over the lane-name gutter), where a zoom anchor would be
  /// meaningless.
  function chartAxisAt(svg, gutter, win, ev) {
    if (!svg) return null;
    var rect = svg.getBoundingClientRect();
    var px = ev.clientX - rect.left;
    if (px < gutter) return null;
    var plotW = Math.max(1, rect.width - CHART_PAD_RIGHT - gutter);
    var f = Math.max(0, Math.min(1, (px - gutter) / plotW));
    return win.from + f * (win.to - win.from);
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
  var TRACE_PAD_RIGHT = CHART_PAD_RIGHT;
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

  // The three below now delegate to the shared geometry above. They stay as
  // named functions because every call site in this section reads `view`, and
  // a `view` is an axis for these purposes — `t_from`, `t_to` and `unit` are
  // exactly what the shared functions take.
  function traceFullWin(view) {
    return chartFullWin(view);
  }

  function traceMinWin(view) {
    return chartMinWin(view.unit);
  }

  function traceClampWin(view, win) {
    return chartClampWin(view, win);
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

  // ---- windowed binning: the server bins, this draws ------------------------
  //
  // **What bounds this drawing is pixels times lanes, not the dataset** — and
  // as of decision 18 that bound is applied before the wire rather than
  // after it. The view this tab loads carries no spans at all: it asks
  // `/api/trace/{study}/{tap}/bins?from&to&width` for the window it is about
  // to draw and gets back at most one occupancy run per pixel column per lane,
  // whatever the capture holds. Decision 10's aggregation is unchanged and now
  // lives in `src/trace.rs` — a span is attributed to every column it covers
  // and counted only on the column it starts in, and a run splits wherever a
  // gap, a below-resolution flag or an open edge changes, so a block still
  // cannot fold an unvouched-for span into a clean-looking bar.
  //
  // **Why the move was worth making at all:** the aggregation had already made
  // *drawing* independent of dataset size, which left the transfer as the only
  // term that still tracked it. The reference capture's 112,801 spans are
  // essentially the whole of the 13 MB JSON the tab used to block on.

  // Flag bits, matching `trace.rs`'s `BIN_*`. The server sets them; this file
  // only reads them.
  var TRACE_F_GAP = 2;
  var TRACE_F_SUBRES = 4;
  var TRACE_F_OPEN = 8;

  /// The bins currently held, and the window they were binned for.
  var traceBins = null;
  /// The window a request is currently outstanding for, as its own key.
  var traceBinsWanted = null;
  /// Sequence number of the newest request. A drag fires one per animation
  /// frame and replies can land out of order; only the newest may become
  /// `traceBins`, or a stale reply would paint an older window over a newer.
  var traceBinsSeq = 0;

  function traceBinsKey(win, cols) {
    return win.from + ":" + win.to + ":" + cols;
  }

  function traceForgetBins() {
    traceBins = null;
    traceBinsWanted = null;
    traceBinsSeq += 1;
  }

  /// The bins for exactly this window, or `null` after asking for them.
  ///
  /// **Nothing is ever drawn from a different window's bins.** A held set
  /// could be rescaled into position and would be approximately right, which
  /// is the one thing this view does not do: a merged block's width would then
  /// be neither its own nor any span's, and "this block's width is not any one
  /// run's duration" would stop being the only caveat on it. So a draw with no
  /// matching bins leaves the last correct picture on screen and returns, and
  /// the reply schedules the redraw. The server holds the decoded capture, so
  /// that round trip is a millisecond or two on loopback.
  function traceBinsFor(view, win, cols) {
    var key = traceBinsKey(win, cols);
    if (traceBins && traceBins.key === key) return traceBins;
    if (traceBinsWanted === key) return null;
    traceBinsWanted = key;
    var seq = (traceBinsSeq += 1);
    fetch(
      "/api/trace/" + encodeURIComponent(view.study_id) + "/" +
      encodeURIComponent(view.tap) + "/bins?from=" + win.from + "&to=" + win.to +
      "&width=" + cols
    )
      .then(function (resp) {
        if (resp.ok) return resp.json();
        return resp.text().then(function (t) { throw new Error(resp.status + " " + t); });
      })
      .then(function (data) {
        if (seq !== traceBinsSeq || traceView !== view) return;
        // The server clamps a window into the capture and says what it
        // clamped to; this file clamps to the same bounds before asking, so
        // the two agree by construction. Said out loud rather than assumed —
        // if they ever stop agreeing the picture stalls with a reason, which
        // is the right failure for a view whose whole point is not drawing
        // things it cannot vouch for.
        if (data.from !== win.from || data.to !== win.to || data.width !== cols) {
          return traceShowError(
            "the server binned " + data.from + "–" + data.to + " at " + data.width +
            " bins, not the " + win.from + "–" + win.to + " at " + cols +
            " this window asked for, so nothing was redrawn"
          );
        }
        var byKey = {};
        (data.lanes || []).forEach(function (l) { byKey[l.key] = l.runs || []; });
        traceBins = { key: key, from: data.from, to: data.to, width: data.width, byKey: byKey };
        traceScheduleDraw();
      })
      .catch(function (e) {
        if (seq !== traceBinsSeq) return;
        traceBinsWanted = null;
        traceShowError("could not bin this window: " + e.message);
      });
    return null;
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
  /// distinguishable rather than "green or not green". Takes a decoded
  /// outcome (`decodeOutcome`), not the raw wire value — this lane's data is
  /// always the flattened shape, but routing it through the same decoder
  /// means an unrecognised value can no longer fall through to `--info`,
  /// which used to read as a fourth, unlabelled, perfectly calm outcome.
  function traceOutcomeColor(decoded) {
    if (decoded.kind === "pass") return "var(--success)";
    if (decoded.kind === "fail") return "var(--danger)";
    if (decoded.kind === "timedout") return "var(--warning)";
    // Unknown: same red stroke as a fail — this must still read as wrong,
    // never as a calm fourth state — but the fill is `tr-cross`, not
    // `tr-gap` (decision 23, amended). `tr-gap` is reserved for a span the
    // *firmware* reported losing records in; an outcome this code failed to
    // parse is a client-side fact with nothing to do with the DUT's ring
    // buffer, and reusing `tr-gap` for it would draw a hardware fault that
    // did not happen. `tr-cross` already means "this view cannot vouch for
    // this span" — true here for the honest reason (unparseable), not the
    // dropped-record one.
    return "var(--danger)";
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
    // 6.9 px per character at 11.5 px IBM Plex Mono — the font's advance is
    // exactly 0.6 em and every glyph shares it, so this is arithmetic, not
    // an estimate. It read 6.6 until 2026-09-19, which under-measured every
    // label by 4.5%; it is checkable now because the font is served by this
    // binary instead of fetched from a CDN that could quietly not answer
    // (`tests/browser/drive_fonts.py` measures it in a real browser and
    // fails if the two drift). Plus the 12 px the label is inset from the
    // plot and a little breathing room.
    var widest = 0;
    lanes.forEach(function (l) { widest = Math.max(widest, l.label.length); });
    traceGutter = Math.max(
      TRACE_GUTTER_MIN,
      Math.min(TRACE_GUTTER_MAX, Math.ceil(widest * 6.9) + 24)
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

    // The grid is fixed from here, so this is the first point at which the
    // right window can be asked for. A miss leaves the previous picture up
    // rather than clearing to an empty axis (`traceBinsFor`).
    var bins = traceBinsFor(view, win, cols);
    if (!bins) return;

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
        "<title>" + escapeHtml(lane.label + " — " + lane.kind + ", " + lane.span_count + " run(s) in the whole capture") +
        "</title>" + escapeHtml(lane.label) + "</text>"
      );
      if (lane.unnamed) {
        parts.push(
          '<line x1="' + (traceGutter - 12 - Math.min(240, lane.label.length * 6.9)) + '" y1="' +
          (mid + 7) + '" x2="' + (traceGutter - 12) + '" y2="' + (mid + 7) +
          '" stroke="var(--text-tertiary)" stroke-width="1" stroke-dasharray="2 2"/>'
        );
      }
      parts.push(
        '<line x1="' + plotLeft + '" y1="' + mid + '" x2="' + plotRight + '" y2="' + mid +
        '" stroke="var(--border)" stroke-width="1" stroke-dasharray="2 4"/>'
      );

      (bins.byKey[lane.key] || []).forEach(function (run) {
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
          var decoded = decodeOutcome(b.outcome, b.reason);
          var color = traceOutcomeColor(decoded);
          // `tr-cross`, not `tr-gap` — see the comment on traceOutcomeColor.
          // The stroke stays `color` (danger-red for "unknown"), so the band
          // reads as wrong first and as "not vouched for" second, rather than
          // borrowing the pattern that specifically means the DUT lost data.
          var fill = decoded.kind === "unknown" ? "url(#tr-cross)" : color;
          var detail =
            "step " + b.index + " · " + b.name + " → " +
            (decoded.kind === "unknown" ? "unrecognised outcome (" + b.outcome + ")" : b.outcome) +
            (decoded.reason ? " (" + decoded.reason + ")" : "") +
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
          // The execution block is always drawn, at a visibility floor of
          // 1.5 px the way a zero-length span is. A step whose whole window
          // is under this capture's ~12 ms clock tie projects to a
          // zero-width band, and a step that ran is not a step that did not.
          {
            hp.push(
              '<rect x="' + bxd + '" y="' + stepTop + '" width="' + Math.max(1.5, bx1 - bxd) + '" height="' +
              TRACE_STEP_H + '" rx="2" fill="' + fill + '" opacity="0.42" stroke="' + color +
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
          // Labelled across the **whole** band, delay included, rather than
          // across the execution half. Zoomed into the head of a study most
          // of a step is its declared delay and the execution is a
          // millisecond sliver — so a label sized to the execution would
          // vanish exactly where a reader most needs to know which step this
          // long wait belongs to.
          var labelW = bx1 - bx0;
          if (labelW > 44) {
            hp.push(
              '<text x="' + (bx0 + labelW / 2) + '" y="' + (stepTop + TRACE_STEP_H / 2 + 4) +
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
    return chartUnitsPerPx(Math.max(640, (svg && svg.clientWidth) || 900), traceGutter, win);
  }

  /// The axis value under a pointer event, in view units. Returns `null` when
  /// the pointer is left of the plot (over the lane-name gutter), where a
  /// zoom anchor would be meaningless.
  function traceAxisAt(view, win, ev) {
    return chartAxisAt(trEl("trace-chart"), traceGutter, win, ev);
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
          '<span class="trace-lane-count mono">' + lane.span_count + "</span>" +
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

  // ---- the Time chart --------------------------------------------------------
  //
  // **One axis, everything on it.** The cards above and below this one each
  // show one stream well and none of them together; this shows the run. Its
  // geometry is the shared one (`chartClampWin` and friends), so a position
  // here and the same position on the Trace chart below are the same pixel.
  //
  // **This file places nothing.** Every `t` arrives already on the axis, from
  // `src/time_chart.rs`, which is the one place embarch-core's receipt clock is
  // crossed onto a capture's own counter. A browser that did its own arithmetic
  // here would be a second implementation of the projection, which is the whole
  // thing decision 18 and suite decision 4 exist to prevent — and the
  // arithmetic in question is the one this suite has already got wrong once, by
  // 46×.

  var tcView = null;
  var tcWin = null;
  var tcBins = null;
  var tcBinsWanted = null;
  var tcBinsSeq = 0;
  var tcDrag = null;
  var tcDrawQueued = false;
  var TC_GUTTER = 210;
  var TC_AXIS_H = 30;
  var TC_ROW_H = 26;
  var TC_MARK_H = 14;
  var TC_SERIES_H = 40;
  var TC_STEP_H = 26;
  var TC_STEP_GAP = 8;
  var TC_BODY_PAD = 18;

  /// The live session's own marks, by lane key — **the only state this file
  /// keeps that the post-hoc chart does not.** A completed study is binned
  /// server-side and this holds nothing; a running one is accumulating here,
  /// bounded by `live_study.rs`'s own per-lane cap, and is binned locally.
  ///
  /// **Binning it here is drawing, not decoding.** The server-side binner
  /// exists because a 13 MB capture must not cross the wire (decision 18); a
  /// live session's marks are already in this browser, arrived one at a time,
  /// and asking the server to re-bin them would be a round trip per frame. The
  /// *rule* is the same one and is stated in both places: a run of bins holding
  /// exactly one mark carries it and is clickable, and anything merged carries
  /// a count and is not.
  var tcLive = null;

  /// Applies one `time_chart` frame from the live feed.
  ///
  /// `replace` is the server saying everything already drawn is on an axis that
  /// no longer exists — a snapshot, or an epoch bump. It is never patched
  /// across: patching would be the chart quietly moving marks somebody has
  /// already looked at.
  function tcApplyLive(frame) {
    if (!frame || !frame.axis) return;
    if (!tcLive || frame.replace || tcLive.epoch !== frame.epoch) {
      tcLive = { epoch: frame.epoch, lanes: {}, order: [] };
    }
    (frame.lanes || []).forEach(function (lane) {
      var held = tcLive.lanes[lane.key];
      if (!held) {
        held = { key: lane.key, label: lane.label, kind: lane.kind, marks: [] };
        tcLive.lanes[lane.key] = held;
        tcLive.order.push(lane.key);
      }
      held.total = lane.total;
      held.dropped = lane.dropped;
      held.pending = lane.pending;
      // Marks arrive already placed and in axis order, and a mark is sent
      // once — the server hands out only what it can place *now* and never
      // re-places what it has sent.
      Array.prototype.push.apply(held.marks, lane.marks || []);
    });

    var axis = frame.axis;
    if (!axis.placeable) {
      // **Waiting is a state, not a failure.** A study that declares a trace
      // draws no axis until its first stamped-and-dated frame, and says which
      // of the three reasons it is still waiting for.
      trEl("tc-body").style.display = "none";
      return tcShowError(axis.note);
    }
    tcShowError("");
    tcView = {
      study_id: lsStudyId,
      live: true,
      axis: {
        unit: axis.unit,
        axis_clock: axis.axis_clock,
        t_from: axis.t_from,
        t_to: axis.t_to,
        projected: axis.projected,
        accuracy_ms: axis.accuracy_ms,
        placeable: true,
        source: axis.source,
        note: axis.note,
      },
      bands: frame.bands || [],
      steps_placeable: !!frame.steps_placeable,
      steps_note: frame.steps_note || "",
      lanes: tcLive.order.map(function (k) {
        var l = tcLive.lanes[k];
        return {
          key: l.key, label: l.label, kind: l.kind,
          total: l.total, placed: l.marks.length,
          before: 0, after: 0, dropped_by_cap: l.dropped,
          note: null,
          pending: l.pending,
        };
      }),
      series: [],
      trace: null,
      axis_epoch: frame.epoch,
      marks_dropped_by_cap: 0,
      notes: (axis.non_monotone
        ? [axis.non_monotone + " frame arrival(s) went backwards against the one before them and " +
           "were refused — an NTP correction mid-capture is the realistic cause, and inserting " +
           "one would land a mark anywhere"]
        : []).concat(
        axis.stale_prefix_dropped
          ? [axis.stale_prefix_dropped + " leading frame(s) were dropped as a stale pre-reset " +
             "prefix, so this axis was redrawn"]
          : []),
    };
    // The window is in axis units and the axis is growing, so a reader who has
    // not zoomed follows the leading edge and one who has stays put.
    if (!tcWin || tcFollowing) {
      tcWin = chartFullWin(tcView.axis);
      tcFollowing = true;
    }
    tcRender();
  }

  /// True while the window is the whole run, so a growing axis keeps the
  /// reader at its leading edge. Any zoom or pan clears it.
  var tcFollowing = true;

  /// Bins the live session's marks over one window, to the same contract the
  /// server's `/marks` answers with.
  function tcBinLive(win, cols) {
    var byKey = {};
    var span = Math.max(1, win.to - win.from);
    (tcLive ? tcLive.order : []).forEach(function (key) {
      var marks = tcLive.lanes[key].marks;
      var counts = new Array(cols).fill(0);
      var one = new Array(cols).fill(null);
      var before = 0, after = 0, visible = 0;
      marks.forEach(function (m) {
        if (m.t < win.from) { before += 1; return; }
        if (m.t > win.to) { after += 1; return; }
        var c = Math.min(cols - 1, Math.floor(((m.t - win.from) / span) * cols));
        counts[c] += 1;
        one[c] = counts[c] === 1 ? m : null;
        visible += 1;
      });
      var runs = [];
      var c = 0;
      while (c < cols) {
        if (counts[c] === 0) { c += 1; continue; }
        var start = c, count = 0, only = null;
        while (c < cols && counts[c] > 0) {
          count += counts[c];
          if (counts[c] === 1 && !only) only = one[c];
          c += 1;
        }
        runs.push({ c0: start, c1: c - 1, count: count, one: count === 1 ? only : null });
      }
      byKey[key] = { key: key, runs: runs, visible: visible, before: before, after: after };
    });
    return { key: tcBinsKey(win, cols), from: win.from, to: win.to, width: cols,
             byKey: byKey, series: {} };
  }

  function tcShowError(message) {
    var el = trEl("tc-error");
    if (!el) return;
    if (!message) {
      el.style.display = "none";
      return;
    }
    el.style.display = "block";
    el.textContent = message;
  }

  function tcForgetBins() {
    tcBins = null;
    tcBinsWanted = null;
    tcBinsSeq += 1;
  }

  /// Loads one study's whole chart. Loading **is** the refresh, the same
  /// contract the Trace card holds: the server rebuilds on every call and
  /// `/marks` answers from what it built until the next one.
  async function tcLoad() {
    var studyId = lsStudyId;
    // A post-hoc load replaces whatever the live feed built, deliberately: the
    // rendered files are authoritative — they carry the whole-capture header
    // pre-pass, the stale-prefix drop over everything and the verified arrival
    // join, none of which a live path can have.
    tcLive = null;
    var body = trEl("tc-body");
    if (!studyId || !body) return;
    tcShowError("");
    var resp;
    try {
      resp = await fetch("/api/time-chart/" + encodeURIComponent(studyId));
    } catch (e) {
      body.style.display = "none";
      return tcShowError("could not reach embarch-ui: " + e.message);
    }
    var text = await resp.text();
    if (!resp.ok) {
      body.style.display = "none";
      return tcShowError(resp.status + " " + text);
    }
    tcView = JSON.parse(text);
    tcWin = null;
    tcForgetBins();
    tcHideDetail();
    tcRender();
  }

  function tcRender() {
    var view = tcView;
    var body = trEl("tc-body");
    if (!view || !body) return;

    // **No axis is a state, not a failure.** A study whose steps carry no
    // stamps and whose taps carry no `core_rx_utc_ms` has nothing to draw on,
    // and positions invented from row order would look exactly like times.
    if (!view.axis.placeable) {
      body.style.display = "none";
      return tcShowError(view.axis.note);
    }
    body.style.display = "block";

    trEl("tc-axis-note").textContent = view.axis.note;
    trEl("tc-steps-note").textContent = view.steps_placeable
      ? ""
      : view.steps_note;
    trEl("tc-steps-note").style.display = view.steps_placeable ? "none" : "block";

    var notes = (view.notes || []).slice();
    if (view.marks_dropped_by_cap) {
      notes.push(
        view.marks_dropped_by_cap +
          " event(s) past a lane's cap are not drawn — the whole capture is still on embarch-core's disk"
      );
    }
    var notesEl = trEl("tc-notes");
    if (notes.length) {
      notesEl.style.display = "block";
      notesEl.textContent = notes.join(" · ");
    } else {
      notesEl.style.display = "none";
    }

    tcScheduleDraw();
  }

  function tcScheduleDraw() {
    if (tcDrawQueued) return;
    tcDrawQueued = true;
    requestAnimationFrame(function () {
      tcDrawQueued = false;
      if (tcView) tcDraw(tcView);
    });
  }

  function tcBinsKey(win, cols) {
    return win.from + ":" + win.to + ":" + cols;
  }

  /// The bins for exactly this window, or `null` after asking for them.
  ///
  /// The same rule the Trace chart holds: **nothing is drawn from a different
  /// window's bins.** A held set could be rescaled into position and would be
  /// approximately right, and "approximately right" is precisely what a chart
  /// built to show a misalignment must never be.
  function tcBinsFor(view, win, cols) {
    var key = tcBinsKey(win, cols);
    // A live session's marks are already here — see `tcLive`. No round trip,
    // and no held set to go stale: it is rebinned from the marks themselves
    // every draw.
    if (view.live) return tcBinLive(win, cols);
    if (tcBins && tcBins.key === key) return tcBins;
    if (tcBinsWanted === key) return null;
    tcBinsWanted = key;
    var seq = (tcBinsSeq += 1);
    fetch(
      "/api/time-chart/" + encodeURIComponent(view.study_id) + "/marks?from=" +
        win.from + "&to=" + win.to + "&width=" + cols
    )
      .then(function (resp) {
        if (resp.ok) return resp.json();
        return resp.text().then(function (t) { throw new Error(resp.status + " " + t); });
      })
      .then(function (data) {
        if (seq !== tcBinsSeq || tcView !== view) return;
        // **An answer from a different axis is refused, never drawn.** The
        // epoch changes only when the axis genuinely improved — a live session
        // dropping a stale prefix, or its header frame arriving and promoting
        // the tier — and drawing a reply from across that change would move
        // marks a reader has already looked at.
        if (data.axis_epoch !== view.axis_epoch) {
          return tcShowError(
            "this chart's axis changed while a window was in flight (epoch " +
              view.axis_epoch + " → " + data.axis_epoch + "); nothing was drawn — press Redraw"
          );
        }
        if (data.from !== win.from || data.to !== win.to || data.width !== cols) {
          return tcShowError(
            "the server binned " + data.from + "–" + data.to + " at " + data.width +
            " bins, not the " + win.from + "–" + win.to + " at " + cols +
            " this window asked for, so nothing was redrawn"
          );
        }
        var byKey = {};
        (data.lanes || []).forEach(function (l) { byKey[l.key] = l; });
        var series = {};
        (data.series || []).forEach(function (l) { series[l.key] = l; });
        tcBins = { key: key, from: data.from, to: data.to, width: data.width, byKey: byKey, series: series };
        tcScheduleDraw();
      })
      .catch(function (e) {
        if (seq !== tcBinsSeq) return;
        tcBinsWanted = null;
        tcShowError("could not bin this window: " + e.message);
      });
    return null;
  }

  /// A lane's colour, by what it carries. Deliberately four distinguishable
  /// hues rather than one: a reader scanning for "the notification that came
  /// in during that step" is scanning by kind first.
  function tcLaneColor(kind) {
    if (kind === "gatt") return "var(--accent)";
    if (kind === "struct") return "var(--info)";
    if (kind === "marker") return "var(--warning)";
    return "var(--text-tertiary)";
  }

  function tcDraw(view) {
    var svg = trEl("tc-chart");
    var head = trEl("tc-head");
    if (!svg || !view.axis.placeable) return;
    if (!tcWin) tcWin = chartFullWin(view.axis);
    var win = chartClampWin(view.axis, tcWin);
    tcWin = win;

    var width = Math.max(640, svg.clientWidth || svg.parentElement.clientWidth || 900);
    var plotLeft = TC_GUTTER;
    var plotRight = width - CHART_PAD_RIGHT;
    var plotW = Math.max(1, plotRight - plotLeft);
    var cols = Math.max(1, Math.round(plotW));
    var span = Math.max(1, win.to - win.from);
    function x(t) {
      return plotLeft + ((t - win.from) / span) * plotW;
    }
    var tier = Math.max(Math.abs(win.to - view.axis.t_from), Math.abs(win.from - view.axis.t_from));
    function at(t) {
      return fmtAxisT(view.axis, t - view.axis.t_from, span, tier);
    }

    var bins = tcBinsFor(view, win, cols);
    if (!bins) return;

    var lanes = view.lanes || [];
    var series = view.series || [];
    var stepped = view.steps_placeable && view.bands && view.bands.length;
    var headH = TC_AXIS_H + (stepped ? TC_STEP_H + TC_STEP_GAP * 2 : 0);

    // ---- body -----------------------------------------------------------
    var parts = [];
    var y = 0;
    var rows = [];
    lanes.forEach(function (l) { rows.push({ lane: l, top: y, h: TC_ROW_H }); y += TC_ROW_H; });
    series.forEach(function (sr) { rows.push({ series: sr, top: y, h: TC_SERIES_H }); y += TC_SERIES_H; });
    var bodyH = Math.max(TC_ROW_H, y) + TC_BODY_PAD;

    parts.push(
      '<defs>' +
      '<pattern id="tc-cluster" width="6" height="6" patternTransform="rotate(45)" patternUnits="userSpaceOnUse">' +
      '<rect width="6" height="6" fill="var(--accent-soft-bg)"/>' +
      '<line x1="0" y1="0" x2="0" y2="6" stroke="var(--accent)" stroke-width="2"/></pattern>' +
      "</defs>"
    );

    var ticks = 6;
    var t;
    for (t = 0; t <= ticks; t += 1) {
      var gx = x(win.from + (span * t) / ticks);
      parts.push(
        '<line x1="' + gx + '" y1="0" x2="' + gx + '" y2="' + (bodyH - 4) +
        '" stroke="var(--border)" stroke-width="1" opacity="0.7"/>'
      );
    }

    var drawn = 0;
    rows.forEach(function (row) {
      var mid = row.top + row.h / 2;
      var lane = row.lane;
      var sr = row.series;
      var label = lane ? lane.label : sr.label;
      var unplaceable = lane ? !!lane.note : !!sr.note;

      parts.push(
        '<text x="' + (TC_GUTTER - 12) + '" y="' + (mid + 4) + '" text-anchor="end" ' +
        'fill="' + (unplaceable ? "var(--text-tertiary)" : "var(--text-primary)") + '" ' +
        'font-size="11.5" font-family="IBM Plex Mono, monospace"' +
        (unplaceable ? ' font-style="italic"' : "") + ">" +
        "<title>" + escapeHtml(
          label + " — " + (lane ? lane.kind : "samples · " + sr.column) + ", " +
          (lane ? lane.total : sr.total) + " event(s) in the whole run, " +
          (lane ? lane.placed : sr.placed) + " of them placeable on this axis" +
          (unplaceable ? " — " + (lane ? lane.note : sr.note) : "")
        ) + "</title>" + escapeHtml(label) + "</text>"
      );
      parts.push(
        '<line x1="' + plotLeft + '" y1="' + mid + '" x2="' + plotRight + '" y2="' + mid +
        '" stroke="var(--border)" stroke-width="1" stroke-dasharray="2 4"/>'
      );

      if (unplaceable) {
        parts.push(
          '<text x="' + (plotLeft + 10) + '" y="' + (mid + 4) + '" fill="var(--text-tertiary)" ' +
          'font-size="11" font-style="italic">' +
          escapeHtml(
            (lane ? lane.total : sr.total) +
            " event(s) this chart cannot place — hover the lane name for why"
          ) + "</text>"
        );
        return;
      }

      if (lane) {
        var binned = bins.byKey[lane.key] || { runs: [], before: 0, after: 0 };
        var color = tcLaneColor(lane.kind);
        var barTop = mid - TC_MARK_H / 2;
        (binned.runs || []).forEach(function (run) {
          var rx = plotLeft + run.c0;
          var rw = Math.max(3, run.c1 - run.c0 + 1);
          var one = run.one;
          var title;
          if (one) {
            title =
              at(one.t) + " · " + (one.sub ? one.sub + " · " : "") + one.label +
              (one.uncertain
                ? " · placed from embarch-core's receipt clock onto this capture's counter, to about " +
                  view.axis.accuracy_ms + " ms"
                : " · exactly where embarch-core received it") +
              " — click to open the row";
          } else {
            var blockFrom = win.from + (run.c0 / cols) * span;
            var blockTo = win.from + ((run.c1 + 1) / cols) * span;
            title =
              run.count + " event(s) of this lane merged into " + (run.c1 - run.c0 + 1) +
              " pixel column(s), " + at(Math.round(blockFrom)) + " → " + at(Math.round(blockTo)) +
              " — zoom in to separate them; no single one of them is drawn here";
          }
          parts.push(
            '<rect x="' + rx + '" y="' + barTop + '" width="' + rw + '" height="' + TC_MARK_H +
            '" rx="2" fill="' + (one ? color : "url(#tc-cluster)") + '"' +
            (one && one.uncertain ? ' opacity="0.72"' : "") +
            (one ? ' class="tc-mark" data-mark-id="' + one.id + '"' : "") +
            "><title>" + escapeHtml(title) + "</title></rect>"
          );
          drawn += 1;
        });
        // A live lane's `pending` is a third count and a different fact from
        // the two gutter ones: not "outside the window you are looking at" but
        // "past the leading edge of the axis itself", which is what the server
        // refuses to place rather than guess at.
        tcGutterCounts(parts, plotLeft, plotRight, mid,
          binned.before, binned.after + (lane.pending || 0), lane.label, "event");
        return;
      }

      // A sample tap: a min/max strip, scaled to what is on screen.
      var sb = bins.series[sr.key] || { bins: [], min: 0, max: 0, before: 0, after: 0 };
      var lo = sb.min;
      var hi = sb.max;
      var range = hi - lo;
      if (!(range > 0)) {
        lo = lo - 0.5;
        range = 1;
      }
      var top = row.top + 4;
      var h = row.h - 10;
      (sb.bins || []).forEach(function (b) {
        var y0 = top + h * (1 - (b.max - lo) / range);
        var y1 = top + h * (1 - (b.min - lo) / range);
        parts.push(
          '<rect x="' + (plotLeft + b.c) + '" y="' + y0 + '" width="1.2" height="' +
          Math.max(1, y1 - y0) + '" fill="var(--success)"/>'
        );
        drawn += 1;
      });
      parts.push(
        '<text x="' + (plotRight - 4) + '" y="' + (top + 10) + '" text-anchor="end" ' +
        'fill="var(--text-tertiary)" font-size="10" font-family="IBM Plex Mono, monospace">' +
        escapeHtml(
          hi.toPrecision(4) + (sr.unit ? " " + sr.unit : "") + " max · " +
          lo.toPrecision(4) + " min in this window"
        ) + "</text>"
      );
      tcGutterCounts(parts, plotLeft, plotRight, mid, sb.before, sb.after, sr.label, "sample");
    });

    if (!rows.length) {
      parts.push(
        '<text x="' + (plotLeft + 12) + '" y="' + (TC_ROW_H / 2 + 4) + '" ' +
        'fill="var(--text-tertiary)" font-size="12">' +
        escapeHtml("This study produced no stream this chart can draw.") + "</text>"
      );
    }

    svg.setAttribute("viewBox", "0 0 " + width + " " + bodyH);
    svg.setAttribute("height", String(bodyH));
    svg.innerHTML = parts.join("");

    // ---- header: the axis and the step row -------------------------------
    if (head) {
      var hp = [];
      var stepTop = TC_AXIS_H + TC_STEP_GAP;
      for (t = 0; t <= ticks; t += 1) {
        var tickAt = win.from + (span * t) / ticks;
        var tx = x(tickAt);
        hp.push(
          '<line x1="' + tx + '" y1="' + (TC_AXIS_H - 6) + '" x2="' + tx + '" y2="' + headH +
          '" stroke="var(--border)" stroke-width="1" opacity="0.7"/>' +
          '<text x="' + tx + '" y="' + (TC_AXIS_H - 10) + '" text-anchor="' +
          (t === 0 ? "start" : t === ticks ? "end" : "middle") + '" ' +
          'fill="var(--text-tertiary)" font-size="10.5" font-family="IBM Plex Mono, monospace">' +
          escapeHtml(at(Math.round(tickAt))) + "</text>"
        );
      }

      if (stepped) {
        hp.push(
          '<text x="' + (TC_GUTTER - 12) + '" y="' + (stepTop + TC_STEP_H / 2 + 4) +
          '" text-anchor="end" fill="var(--text-secondary)" font-size="11.5" ' +
          'font-family="IBM Plex Mono, monospace">' +
          escapeHtml(view.axis.projected
            ? "study step (±" + view.axis.accuracy_ms + " ms)"
            : "study step") + "</text>"
        );
        view.bands.forEach(function (b) {
          if (b.to < win.from || b.from > win.to) return;
          var bx0 = Math.max(plotLeft, Math.min(plotRight, x(b.from)));
          var bx1 = Math.max(plotLeft, Math.min(plotRight, x(b.to)));
          var bxd = Math.max(plotLeft, Math.min(plotRight, x(b.exec_from)));
          var decoded = decodeOutcome(b.outcome, b.reason);
          var color = traceOutcomeColor(decoded);
          var detail =
            "step " + b.index + " · " + b.name + " → " + b.outcome +
            (decoded.reason ? " (" + decoded.reason + ")" : "") +
            (b.delay_before_ms > 0 ? " · " + b.delay_before_ms + " ms declared delay first" : "") +
            (b.clipped_start ? " · began before this axis starts" : "") +
            (b.clipped_end ? " · ended after this axis ends" : "");
          if (bxd > bx0 + 0.5) {
            hp.push(
              '<rect x="' + bx0 + '" y="' + stepTop + '" width="' + (bxd - bx0) + '" height="' +
              TC_STEP_H + '" fill="var(--bg-surface-inset)" stroke="var(--border)" ' +
              'stroke-width="1"><title>' + escapeHtml(detail) + "</title></rect>"
            );
          }
          hp.push(
            '<rect x="' + bxd + '" y="' + stepTop + '" width="' + Math.max(1.5, bx1 - bxd) +
            '" height="' + TC_STEP_H + '" rx="2" fill="' + color +
            '" opacity="0.42" stroke="' + color + '" stroke-width="1"><title>' +
            escapeHtml(detail) + "</title></rect>"
          );
          var labelW = bx1 - bx0;
          if (labelW > 44) {
            hp.push(
              '<text x="' + (bx0 + labelW / 2) + '" y="' + (stepTop + TC_STEP_H / 2 + 4) +
              '" text-anchor="middle" fill="var(--text-primary)" font-size="10.5" ' +
              'font-family="IBM Plex Mono, monospace" pointer-events="none">' +
              escapeHtml(traceFitLabel(b.name, labelW)) + "</text>"
            );
          }
        });
      }

      head.setAttribute("viewBox", "0 0 " + width + " " + headH);
      head.setAttribute("height", String(headH));
      head.style.width = width + "px";
      head.innerHTML = hp.join("");
    }

    tcUpdateReadout(view, win, drawn);
  }

  /// The counts at each end of a lane: events that are real, placed, and
  /// outside the window a reader is looking at.
  ///
  /// **Two numbers, not one.** "214 before this trace opened" says to widen the
  /// tap's scope earlier; the same total as one number says nothing.
  function tcGutterCounts(parts, plotLeft, plotRight, mid, before, after, label, noun) {
    if (before) {
      parts.push(
        '<text x="' + (plotLeft + 3) + '" y="' + (mid - 9) + '" fill="var(--text-tertiary)" ' +
        'font-size="10" font-family="IBM Plex Mono, monospace">&#9666;' + before +
        "<title>" + escapeHtml(before + " " + noun + "(s) of " + label +
          " fall before this window — they are drawn nowhere here rather than stacked at the edge") +
        "</title></text>"
      );
    }
    if (after) {
      parts.push(
        '<text x="' + (plotRight - 3) + '" y="' + (mid - 9) + '" text-anchor="end" ' +
        'fill="var(--text-tertiary)" font-size="10" font-family="IBM Plex Mono, monospace">' +
        after + "&#9656;<title>" + escapeHtml(after + " " + noun + "(s) of " + label +
          " fall after this window") + "</title></text>"
      );
    }
  }

  function tcUpdateReadout(view, win, drawn) {
    var el = trEl("tc-window");
    if (!el) return;
    var full = chartFullWin(view.axis);
    var extent = full.to - full.from;
    var w = win.to - win.from;
    var span = Math.max(1, w);
    el.textContent =
      (w < extent
        ? "showing " + fmtSpanLen(view.axis, w) + " of " + fmtSpanLen(view.axis, extent) + " — " +
          fmtAxisT(view.axis, win.from - view.axis.t_from, span, win.to - view.axis.t_from) +
          " to " + fmtAxisT(view.axis, win.to - view.axis.t_from, span, win.to - view.axis.t_from)
        : "showing the whole run, " + fmtSpanLen(view.axis, extent)) +
      " · " + (view.lanes.length + view.series.length) + " lanes · " + drawn + " marks drawn · " +
      view.axis.axis_clock;
  }

  // ---- opening one mark ------------------------------------------------------

  function tcHideDetail() {
    var el = trEl("tc-detail");
    if (el) el.style.display = "none";
  }

  /// Opens one mark: the row it **is**, fetched rather than carried.
  ///
  /// The id encodes the lane and the row index, so this is the same row the
  /// Data card shows for that record — not a second rendering that could
  /// disagree with it.
  async function tcOpenMark(id) {
    var el = trEl("tc-detail");
    if (!el || !tcView) return;
    el.style.display = "block";
    el.innerHTML = '<div class="card-title">Opening&hellip;</div>';
    var resp = await fetch(
      "/api/time-chart/" + encodeURIComponent(tcView.study_id) + "/mark/" + encodeURIComponent(id)
    );
    var text = await resp.text();
    if (!resp.ok) {
      el.innerHTML =
        '<div class="card-title">This mark has no row to open</div>' +
        '<p class="sd-error">' + escapeHtml(resp.status + " " + text) + "</p>";
      return;
    }
    var d = JSON.parse(text);
    var head =
      '<div class="sd-toolbar"><div class="card-title" style="margin:0;">' +
      escapeHtml((d.tap || d.lane) + (d.row_index === undefined ? "" : " · row " + d.row_index)) +
      '</div><div class="sd-toolbar-actions"><button id="tc-detail-close" class="btn">Close</button></div></div>';
    var where = d.mark
      ? '<p class="placeholder-note" style="margin:8px 0;">' +
        escapeHtml(
          "at " + fmtAxisT(tcView.axis, d.mark.t - tcView.axis.t_from,
            Math.max(1, tcView.axis.t_to - tcView.axis.t_from),
            tcView.axis.t_to - tcView.axis.t_from) +
          (d.mark.core_rx_utc_ms
            ? " · embarch-core received it at " + new Date(d.mark.core_rx_utc_ms).toISOString()
            : "") +
          (d.mark.uncertain
            ? " · placed across two clocks, to about " + tcView.axis.accuracy_ms + " ms"
            : "")
        ) + "</p>"
      : "";
    var bodyHtml;
    if (d.columns) {
      bodyHtml =
        '<div class="table-scroll"><table class="data-table"><tbody>' +
        d.columns.map(function (c, i) {
          return "<tr><th style=\"width:180px;\">" + escapeHtml(c) + "</th><td class=\"mono\">" +
            escapeHtml(d.row[i] === undefined ? "" : d.row[i]) + "</td></tr>";
        }).join("") +
        "</tbody></table></div>";
    } else {
      bodyHtml = '<p class="placeholder-note">' + escapeHtml(d.note || "") + "</p>";
    }
    el.innerHTML = head + where + bodyHtml;
    var close = trEl("tc-detail-close");
    if (close) close.addEventListener("click", tcHideDetail);
  }

  // ---- navigation ------------------------------------------------------------

  function tcOnWheel(ev) {
    if (!tcView || !tcWin) return;
    if (ev.shiftKey) return;
    var anchor = chartAxisAt(trEl("tc-chart"), TC_GUTTER, tcWin, ev);
    if (anchor === null) return;
    ev.preventDefault();
    var delta = ev.deltaY * (ev.deltaMode === 1 ? 33 : ev.deltaMode === 2 ? 700 : 1);
    var factor = Math.exp(delta * 0.0028);
    var w = tcWin.to - tcWin.from;
    var next = w * factor;
    var frac = (anchor - tcWin.from) / w;
    tcWin = chartClampWin(tcView.axis, {
      from: anchor - frac * next,
      to: anchor + (1 - frac) * next,
    });
    tcFollowing = false;
    tcScheduleDraw();
  }

  function tcOnPointerDown(ev) {
    if (!tcView || !tcWin) return;
    if (ev.button !== 0) return;
    // A click on a mark opens it rather than starting a drag — a mark is 3 px
    // wide and a drag that began on one would swallow every click.
    var mark = ev.target && ev.target.getAttribute && ev.target.getAttribute("data-mark-id");
    if (mark) {
      tcOpenMark(mark);
      return;
    }
    if (chartAxisAt(trEl("tc-chart"), TC_GUTTER, tcWin, ev) === null) return;
    tcDrag = { x: ev.clientX, win: { from: tcWin.from, to: tcWin.to } };
    var plot = trEl("tc-plot");
    if (plot) plot.classList.add("is-panning");
    ev.preventDefault();
  }

  function tcOnPointerMove(ev) {
    if (!tcDrag || !tcView) return;
    var svg = trEl("tc-chart");
    var perPx = chartUnitsPerPx(
      Math.max(640, (svg && svg.clientWidth) || 900), TC_GUTTER, tcDrag.win
    );
    var dt = (ev.clientX - tcDrag.x) * perPx;
    tcWin = chartClampWin(tcView.axis, {
      from: tcDrag.win.from - dt,
      to: tcDrag.win.to - dt,
    });
    tcFollowing = false;
    tcScheduleDraw();
  }

  function tcOnPointerUp() {
    tcDrag = null;
    var plot = trEl("tc-plot");
    if (plot) plot.classList.remove("is-panning");
  }

  function tcFit() {
    if (!tcView) return;
    tcWin = chartFullWin(tcView.axis);
    // Fitting a growing axis means following it again.
    tcFollowing = true;
    tcScheduleDraw();
  }

  function initTimeChart() {
    var fit = trEl("tc-fit");
    if (fit) fit.addEventListener("click", tcFit);
    var reload = trEl("tc-reload");
    if (reload) reload.addEventListener("click", tcLoad);
    var plot = trEl("tc-plot");
    if (plot) {
      plot.addEventListener("wheel", tcOnWheel, { passive: false });
      plot.addEventListener("pointerdown", tcOnPointerDown);
      plot.addEventListener("pointermove", tcOnPointerMove);
      plot.addEventListener("pointerup", tcOnPointerUp);
      plot.addEventListener("pointercancel", tcOnPointerUp);
      plot.addEventListener("dblclick", function (ev) {
        ev.preventDefault();
        tcFit();
      });
    }
    var pending = null;
    window.addEventListener("resize", function () {
      if (!tcView) return;
      clearTimeout(pending);
      pending = setTimeout(function () { tcScheduleDraw(); }, 120);
    });
  }

  function initTraceTab() {
    if (!trEl("trace-chart")) return;
    // Redraws the tap already selected. The study is the Live Study tab's —
    // this chart is a card on that page now, not a view somebody addresses
    // on its own, so the `#trace?study=…&tap=…` deep link that used to reach
    // it is gone with the tab it named. `#live-study` still selects the tab,
    // like every other.
    trEl("trace-load").addEventListener("click", traceLoadView);
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

  // The project control and its dialog: shell furniture, wired with the rest
  // of the shell rather than inside a tab, because the control is on screen
  // on every tab and the dialog is opened from two of them.
  function initProjectPicker() {
    const button = document.getElementById("project-button");
    if (button) button.addEventListener("click", openProjectDialog);
    const cancel = document.getElementById("project-cancel");
    if (cancel) cancel.addEventListener("click", closeProjectDialog);
    const backdrop = document.getElementById("project-dialog-backdrop");
    if (backdrop) backdrop.addEventListener("click", closeProjectDialog);
    const open = document.getElementById("project-open");
    if (open) open.addEventListener("click", function () { sdOpenProject(); });
    const path = document.getElementById("project-path");
    if (path) {
      path.addEventListener("keydown", function (ev) {
        if (ev.key === "Enter") sdOpenProject();
      });
    }
    const recents = document.getElementById("project-recents");
    if (recents) {
      recents.addEventListener("change", function (ev) {
        const opt = ev.target.selectedOptions[0];
        if (!opt || !opt.value) return;
        document.getElementById("project-path").value = opt.value;
        // Picking a recent project opens it: an extra click on Open would be
        // asking twice for the same decision.
        sdOpenProject(opt.value);
      });
    }
  }

  document.addEventListener("DOMContentLoaded", () => {
    initNav();
    initProjectPicker();
    initEnrollOnDiagram();
    initSignals();
    initTopologyTab();
    initStudyDesignerTab();
    initLiveStudyTab();
    initTimeChart();
    initTraceTab();
    initDebugTab();
    initEvents();
    const toggle = document.querySelector(".theme-toggle");
    if (toggle) toggle.addEventListener("click", toggleTheme);
  });
})();
