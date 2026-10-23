// embarch-ui Desk tab: one card per work item with recent activity, its time
// tile inside it (embarch-desk-proposal.md). Everything comes from
// embarch-desk through this server's /api/desk proxy; nothing is computed here
// that the desk already knows. Zero-build (decision 2): plain JS, no library.
//
// Two kinds of write, and the tab keeps them apart: a status move (an issue's
// state, a tile's ✓, ✕) happens on the click; anything carrying prose (a
// tile's note, a Slack reply) is shown in an editable box first and is sent
// only as the user left it.

(function () {
  "use strict";

  var panel = document.querySelector('.tab-panel[data-tab="desk"]');
  if (!panel) return;
  var cardsEl = document.getElementById("desk-cards");
  var errorEl = document.getElementById("desk-error");
  var slackEl = document.getElementById("desk-slack");
  var settingsEl = document.getElementById("desk-settings");
  var draftsEl = document.getElementById("desk-drafts");
  var elsewhereEl = document.getElementById("desk-elsewhere");
  var dayBody = document.getElementById("desk-day-body");
  var dayDate = document.getElementById("desk-day-date");
  var dayTotal = document.getElementById("desk-day-total");
  var formEl = document.getElementById("desk-form");
  var formBackdrop = document.getElementById("desk-form-backdrop");
  var newDeskSel = document.getElementById("desk-new-desk");
  var pickedDay = null;    // a date the user picked; null follows today
  var lastOpenId;          // the open desk the last pushed view named
  var timerEl = document.getElementById("desk-timer");
  var dialogEl = document.getElementById("desk-dialog");
  var backdropEl = document.getElementById("desk-dialog-backdrop");

  var view = null;
  var states = {};           // item id -> [{id, name, type}], loaded on first open of the dropdown
  var targets = null;        // Harvest projects/tasks, loaded when a tile opens
  var openTileId = null;

  // ---------------------------------------------------------------- helpers

  function esc(v) {
    return String(v == null ? "" : v).replace(/[&<>"']/g, function (ch) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch];
    });
  }

  function api(method, path, body) {
    var opts = { method: method, headers: {} };
    if (body !== undefined) {
      opts.headers["Content-Type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    return fetch("/api/desk" + path, opts).then(function (r) {
      return r.json().catch(function () { return {}; }).then(function (j) {
        if (!r.ok) {
          var e = new Error(j.error || ("HTTP " + r.status));
          e.status = r.status;
          throw e;
        }
        return j;
      });
    });
  }

  function hours(h) {
    var n = Number(h) || 0;
    var hh = Math.floor(n);
    var mm = Math.round((n - hh) * 60);
    if (mm === 60) { hh += 1; mm = 0; }
    return hh + "h" + (mm < 10 ? "0" : "") + mm;
  }

  function dayLabel(date) {
    if (view && date === view.today) return "Today";
    var d = new Date(date + "T12:00:00");
    return isNaN(d) ? date : d.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
  }

  function ago(ms) {
    var s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 90) return "just now";
    if (s < 3600) return Math.round(s / 60) + " min ago";
    if (s < 86400) return Math.round(s / 3600) + " h ago";
    return Math.round(s / 86400) + " d ago";
  }

  function showError(msg) {
    if (!msg) { errorEl.hidden = true; return; }
    errorEl.hidden = false;
    errorEl.innerHTML = '<div class="card-title" style="color:var(--danger);">Desk</div><p class="placeholder-note">' + esc(msg) + "</p>";
  }

  function flash(msg) {
    showError(msg);
    setTimeout(function () { if (errorEl.textContent.indexOf(msg) >= 0) showError(null); }, 8000);
  }

  // ---------------------------------------------------------------- render

  function prsHtml(prs) {
    if (!prs || !prs.length) return "";
    return '<div class="desk-prs">' + prs.map(function (p) {
      var st = p.state === "merged" ? "merged" : (p.draft ? "draft" : p.state);
      return '<div class="desk-pr"><a href="' + esc(p.url) + '" target="_blank" rel="noopener">' +
        esc(p.repo.split("/").pop()) + "#" + esc(p.number) + "</a> " +
        '<span class="desk-pr-title">' + esc(p.title) + "</span> " +
        '<span class="badge badge-neutral">' + esc(st) + "</span></div>";
    }).join("") + "</div>";
  }

  function benchHtml(bench) {
    if (!bench || !bench.length) return "";
    return bench.map(function (b) {
      return esc(b.tool.replace(/_/g, " ")) + " ×" + b.count + (b.failed ? ' <span style="color:var(--danger);">(' + b.failed + " failed)</span>" : "");
    }).join(" · ");
  }

  function tileHtml(t) {
    var lines = (t.lines || []).length;
    var ready = t.harvest && t.note;
    return '<div class="desk-tile' + (t.date === view.today ? " today" : "") + '" data-tile="' + esc(t.id) + '">' +
      '<button class="desk-tile-body" type="button" data-open-tile="' + esc(t.id) + '" title="Open: lines, note, Harvest project">' +
      '<span class="desk-tile-date">' + esc(dayLabel(t.date)) + "</span>" +
      '<span class="desk-tile-hours">' + hours(t.hours) + "</span>" +
      '<span class="desk-tile-sub">' + (t.logged_hours ? hours(t.logged_hours) + " already in Harvest" : lines + " line" + (lines === 1 ? "" : "s") + (ready ? " · ready" : "")) + "</span>" +
      "</button>" +
      '<div class="desk-tile-actions">' +
      '<button class="desk-ok" type="button" data-approve="' + esc(t.id) + '" title="Add to Harvest">✓</button>' +
      '<button class="desk-no" type="button" data-discard="' + esc(t.id) + '" title="Discard">✕</button>' +
      "</div></div>";
  }

  function cardHtml(c) {
    var issue = c.issue;
    var head = issue
      ? '<a class="desk-key" href="' + esc(issue.url) + '" target="_blank" rel="noopener">' + esc(issue.key) + "</a>" +
        '<span class="chip desk-account" title="Linear connector">' + esc(issue.account) + "</span>"
      : '<span class="desk-key desk-key-none">no issue</span>';
    var status = issue
      ? '<select class="desk-status" data-status="' + esc(c.id) + '" title="Move the issue">' +
        (states[c.id] || [{ id: issue.state_id, name: issue.state_name }]).map(function (s) {
          return '<option value="' + esc(s.id) + '"' + (s.id === issue.state_id ? " selected" : "") + ">" + esc(s.name) + "</option>";
        }).join("") + "</select>"
      : "";
    var s = c.sessions || {};
    var meta = [];
    if (s.total) meta.push(s.total + " session" + (s.total === 1 ? "" : "s") + (s.live ? " (" + s.live + " live)" : ""));
    var bench = benchHtml(c.bench);
    if (bench) meta.push(bench);
    meta.push("active " + ago(c.last_activity_ms));
    var tiles = (c.tiles || []).map(tileHtml).join("") ||
      '<div class="desk-tile-empty">' + (c.logged_today
        ? hours(c.logged_today) + " already in Harvest today. New session time lands here."
        : "No time yet today. Open it in VS Code and the session\'s time lands here.") + "</div>";
    return '<div class="card desk-card' + (c.pinned ? " pinned" : "") + '" data-card="' + esc(c.id) + '">' +
      '<div class="desk-card-head"><div class="desk-card-id">' + head + "</div>" + status + "</div>" +
      '<div class="desk-card-title">' + esc(c.title) + "</div>" +
      prsHtml(c.prs) +
      '<div class="desk-card-meta">' + meta.join(" · ") + "</div>" +
      '<div class="desk-tiles">' + tiles + "</div>" +
      '<div class="desk-card-actions">' +
      '<button class="btn btn-tiny" type="button" data-launch="' + esc(c.id) + '" title="' + esc(c.repo_path || "pick the repo folder first") + '">Open in VS Code</button>' +
      (view.timer && view.timer.item_id === c.id
        ? '<button class="btn btn-tiny desk-timer-on" type="button" data-timer-stop="1">Stop timer</button>'
        : '<button class="btn btn-tiny" type="button" data-timer-start="' + esc(c.id) + '" title="Time away from Claude Code: the bench, a call, a meeting">Start timer</button>') +
      (issue ? "" : '<button class="btn btn-tiny" type="button" data-link="' + esc(c.id) + '">Link issue</button>') +
      '<button class="btn btn-tiny" type="button" data-folder="' + esc(c.id) + '">' + (c.repo_path ? "Repo…" : "Pick repo…") + "</button>" +
      '<button class="btn btn-tiny" type="button" data-pin="' + esc(c.id) + '" data-value="' + (!c.pinned) + '">' + (c.pinned ? "Unpin" : "Pin") + "</button>" +
      '<button class="btn btn-tiny" type="button" data-archive="' + esc(c.id) + '">Archive</button>' +
      "</div></div>";
  }

  function renderSlackBar() {
    var sl = view.slack || {};
    var parts = [];
    parts.push((sl.mentions || 0) + " mention" + (sl.mentions === 1 ? "" : "s"));
    parts.push((sl.dms || 0) + " DM" + (sl.dms === 1 ? "" : "s"));
    if (sl.unread) parts.push(sl.unread + " unread");
    document.getElementById("desk-slack-counts").textContent = "Slack · " + parts.join(" · ");
    var w = document.getElementById("desk-waiting");
    if (view.sessions_waiting) {
      w.hidden = false;
      w.textContent = view.sessions_waiting + " session" + (view.sessions_waiting === 1 ? "" : "s") + " waiting for a work item";
    } else {
      w.hidden = true;
    }
  }

  function renderSlack() {
    if (slackEl.hidden) return;
    var sl = view.slack || {};
    var items = sl.items || [];
    var errs = (sl.errors || []).map(function (e) { return '<p class="placeholder-note" style="color:var(--danger);">' + esc(e) + "</p>"; }).join("");
    if (!items.length) {
      slackEl.innerHTML = '<div class="card-title">Slack</div>' + errs +
        '<p class="placeholder-note">' + ((view.connectors || []).some(function (c) { return c.kind === "slack"; }) ? "Nothing in the last three days." : "No Slack workspace connected — add one under Connectors.") + "</p>";
      return;
    }
    slackEl.innerHTML = '<div class="card-title">Slack — mentions and DMs, last three days</div>' + errs +
      items.map(function (m, i) {
        var when = new Date(parseFloat(m.ts) * 1000);
        return '<div class="desk-msg' + (m.unread ? " unread" : "") + '">' +
          '<div class="desk-msg-head"><span class="desk-msg-from">' + esc(m.user_name) + "</span>" +
          '<span class="desk-msg-where">' + (m.kind === "dm" ? "DM" : "#" + esc(m.channel_name)) + " · " + esc(m.account) + " · " + esc(when.toLocaleString()) + "</span>" +
          '<span class="desk-msg-acts"><a href="' + esc(m.permalink) + '" target="_blank" rel="noopener">Open</a> ' +
          '<button class="btn btn-tiny" type="button" data-draft="' + i + '">Draft reply</button></span></div>' +
          '<div class="desk-msg-text">' + esc(m.text) + "</div>" +
          '<div class="desk-reply" data-reply-box="' + i + '" hidden></div></div>';
      }).join("");
  }

  function renderSettings() {
    if (settingsEl.hidden) return;
    var rows = (view.connectors || []).map(function (c) {
      return '<tr><td>' + esc(c.kind) + "</td><td class=\"mono\">" + esc(c.label) + "</td><td>" +
        (c.ok ? '<span class="badge badge-success">ok</span> ' : '<span class="badge badge-danger">failing</span> ') +
        '<span class="desk-detail">' + esc(c.detail) + "</span></td><td>" +
        (c.label === "gh login" ? '<span class="desk-detail">your gh login</span>' :
          '<button class="btn-icon-danger btn" type="button" data-disconnect="' + esc(c.kind) + "/" + esc(c.label) + '">Remove</button>') +
        "</td></tr>";
    }).join("") || '<tr><td colspan="4" class="placeholder-note">No connectors yet.</td></tr>';
    settingsEl.innerHTML =
      '<div class="card-title">Connectors</div>' +
      '<table class="data-table"><thead><tr><th>Service</th><th>Account</th><th>Status</th><th></th></tr></thead><tbody>' + rows + "</tbody></table>" +
      '<div class="desk-form">' +
      '<select id="desk-conn-kind" class="desk-input"><option value="linear">Linear</option><option value="harvest">Harvest</option><option value="github">GitHub</option><option value="slack">Slack</option></select>' +
      '<input id="desk-conn-label" class="desk-input" type="text" placeholder="Name, e.g. the workspace" />' +
      '<input id="desk-conn-token" class="desk-input" type="password" placeholder="Token" autocomplete="off" />' +
      '<button class="btn btn-primary" id="desk-conn-add" type="button">Connect</button></div>' +
      '<p class="desk-help">Linear: Settings → Security &amp; access → Personal API keys, one per workspace. ' +
      "Harvest: id.getharvest.com/developers → Create new personal access token. " +
      "GitHub: nothing to do if you are logged in to <span class=\"mono\">gh</span>. " +
      "Slack: create an app in your workspace from embarch-desk's <span class=\"mono\">slack-manifest.json</span>, install it, and paste its User OAuth Token (<span class=\"mono\">xoxp-…</span>). " +
      "Tokens stay in <span class=\"mono\">~/.config/embarch-desk/connectors.toml</span>, owner-only.</p>" +
      '<div class="card-title" style="margin-top:18px;">Never tracked</div>' +
      '<p class="desk-help">Sessions under these folders make no time and are never asked for a work item — open-source work, the agent fleet.</p>' +
      '<textarea id="desk-ignore" class="desk-input desk-textarea" rows="3" spellcheck="false">' + esc((view.ignore || []).join("\n")) + "</textarea>" +
      '<div><button class="btn" id="desk-ignore-save" type="button">Save</button></div>';
  }

  function renderTimer() {
    var t = view.timer;
    if (!t) { timerEl.hidden = true; return; }
    timerEl.hidden = false;
    timerEl.innerHTML = '<span class="desk-timer-dot"></span><span>Timer on <b>' + esc(t.title || t.item_id) + "</b> since " +
      esc(new Date(t.started_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })) + " · " + hours(t.seconds / 3600) + "</span>" +
      '<button class="btn btn-tiny" type="button" data-timer-stop="1">Stop</button>';
  }

  // What agents asked to write. Nothing here has been sent: Send posts the
  // text as it stands in the box, Discard drops it (decision 15).
  function renderDrafts() {
    var drafts = view.drafts || [];
    if (!drafts.length) { draftsEl.hidden = true; draftsEl.innerHTML = ""; return; }
    draftsEl.hidden = false;
    draftsEl.innerHTML = '<div class="card-title">Drafts from agents — nothing is sent until you press Send</div>' +
      drafts.map(function (d) {
        var what = d.kind === "linear_issue"
          ? "New Linear issue in <b>" + esc(d.team_key) + "</b>"
          : "Comment on <b>" + esc(d.issue_key) + "</b>";
        return '<div class="desk-draft" data-draft-id="' + esc(d.id) + '">' +
          '<div class="desk-draft-head">' + what + ' <span class="desk-detail">' + esc(new Date(d.created_ms).toLocaleString()) + "</span></div>" +
          (d.kind === "linear_issue" ? '<input class="desk-input desk-draft-title" type="text" value="' + esc(d.title || "") + '" />' : "") +
          '<textarea class="desk-input desk-textarea desk-draft-body" rows="4">' + esc(d.body) + "</textarea>" +
          '<div class="desk-form"><button class="btn btn-primary btn-tiny" type="button" data-draft-send="' + esc(d.id) + '">Send</button>' +
          '<button class="btn btn-tiny" type="button" data-draft-discard="' + esc(d.id) + '">Discard</button></div></div>';
      }).join("");
  }

  function renderHead() {
    var d = view.open_desk;
    document.getElementById("desk-head-name").textContent = d ? d.name : "no desk";
    document.getElementById("desk-edit").hidden = !d;
    var desks = view.desks || [];
    var keep = newDeskSel.value;
    newDeskSel.hidden = !desks.length;
    newDeskSel.innerHTML = '<option value="">desk for a standalone card…</option>' +
      desks.map(function (k) { return '<option value="' + esc(k.id) + '">' + esc(k.name) + "</option>"; }).join("") +
      '<option value="none">no desk</option>';
    newDeskSel.value = keep || (d ? d.id : "");
  }

  function renderElsewhere() {
    var tiles = view.elsewhere || [];
    if (!tiles.length) { elsewhereEl.hidden = true; elsewhereEl.innerHTML = ""; return; }
    elsewhereEl.hidden = false;
    elsewhereEl.innerHTML = "<span>Unsent on other desks:</span>" + tiles.map(function (t) {
      return '<button class="desk-mini" type="button" data-open-tile="' + esc(t.id) + '" title="Open the tile">' +
        esc(t.desk || "no desk") + " · " + esc(t.item_title) + " · " + esc(dayLabel(t.date)) + " · <b>" + hours(t.hours) + "</b></button>";
    }).join("");
  }

  // "Today's work": every work item with time, a line, bench activity or a
  // session that day, grouped by desk, the open one first — links out, what
  // was done, status and sessions. A picked date is fetched; today is pushed.
  var lastDay = null;

  function entryHtml(e) {
    var key = e.account_id + "/" + e.id;
    return '<li class="desk-entry" data-entry="' + esc(key) + '"><span class="desk-entry-hours">' + hours(e.hours) + "</span> " +
      '<span class="desk-detail">' + esc(e.task) + (e.running ? " · running" : "") + "</span> " +
      '<span class="desk-entry-notes">' + esc(e.notes) + "</span> " +
      '<button class="btn btn-tiny" type="button" data-entry-edit="' + esc(key) + '">Edit</button>' +
      '<button class="btn btn-tiny desk-no" type="button" data-entry-delete="' + esc(key) + '">Delete</button></li>';
  }

  function findEntry(key) {
    var out = null;
    ((lastDay && lastDay.groups) || []).forEach(function (g) {
      g.items.forEach(function (r) {
        (r.harvest_entries || []).forEach(function (e) { if (e.account_id + "/" + e.id === key) out = e; });
      });
    });
    return out;
  }

  function renderDay(day) {
    if (!day) return;
    lastDay = day;
    dayTotal.textContent = (day.groups || []).length
      ? "total " + hours(day.total_hours) + " · " + hours(day.logged_hours) + " in Harvest · " + hours(day.unsent_hours) + " unsent"
      : "";
    if (!pickedDay) dayDate.value = view.today;
    var groups = day.groups || [];
    if (!groups.length) {
      dayBody.innerHTML = '<p class="placeholder-note">Nothing recorded on ' + esc(dayLabel(day.date)) + ".</p>";
      return;
    }
    dayBody.innerHTML = groups.map(function (g) {
      return '<div class="desk-day-group"><div class="desk-day-desk">' + esc(g.name) + "</div>" + g.items.map(function (r) {
        var issue = r.issue;
        var title = issue
          ? '<a href="' + esc(issue.url) + '" target="_blank" rel="noopener" title="Open in Linear">' + esc(issue.key) + " · " + esc(r.title) + "</a>"
          : esc(r.title);
        var meta = [];
        if (issue && issue.state_name) meta.push('<span class="badge badge-neutral">' + esc(issue.state_name) + "</span>");
        (r.prs || []).forEach(function (pr) {
          meta.push('<a href="' + esc(pr.url) + '" target="_blank" rel="noopener" title="' + esc(pr.title) + '">' +
            esc(pr.repo.split("/").pop()) + "#" + esc(pr.number) + "</a>");
        });
        if (r.sessions) meta.push(r.sessions + " session" + (r.sessions === 1 ? "" : "s") + (r.live ? " (" + r.live + " live)" : ""));
        if (r.bench) meta.push(r.bench + " bench call" + (r.bench === 1 ? "" : "s") + (r.bench_failed ? ', <span style="color:var(--danger);">' + r.bench_failed + " failed</span>" : ""));
        if (r.logged_hours) meta.push('<span class="badge badge-success">' + hours(r.logged_hours) + " in Harvest</span>");
        if (r.status === "open" && r.hours > 0) meta.push('<span class="badge badge-warning">' + hours(r.hours) + " unsent</span>");
        if (r.status === "discarded") meta.push('<span class="badge badge-neutral">tile discarded</span>');
        var lines = (r.lines || []).map(function (l) { return "<li>" + esc(l.text) + "</li>"; }).join("");
        var entries = (r.harvest_entries || []).map(entryHtml).join("");
        var shown = (r.logged_hours || 0) + (r.status === "open" ? (r.hours || 0) : 0);
        return '<div class="desk-day-row"><div class="desk-day-title">' + title + "</div>" +
          '<div class="desk-day-hours">' + hours(shown) + "</div>" +
          (entries ? '<ul class="desk-entries">' + entries + "</ul>" : "") +
          '<div class="desk-day-meta">' + meta.join(" ") + "</div>" +
          (lines ? '<ul class="desk-day-lines">' + lines + "</ul>" : "") +
          (r.note ? '<div class="desk-day-note">' + esc(r.note) + "</div>" : "") + "</div>";
      }).join("") + "</div>";
    }).join("");
  }

  // After an edit, read the day again from Harvest, and the cards with it.
  function refreshDay() {
    var date = pickedDay || (view && view.today);
    return api("GET", "/day/" + encodeURIComponent(date)).then(function (d) {
      if (!pickedDay && view) view.day = d;
      renderDay(d);
      return load();
    });
  }

  function loadDay(date) {
    pickedDay = date && date !== (view && view.today) ? date : null;
    if (!pickedDay) { renderDay(view && view.day); return; }
    api("GET", "/day/" + encodeURIComponent(pickedDay)).then(renderDay).catch(function (e) { flash(e.message); });
  }

  // ---------------------------------------------------------------- the desk form
  var formData = { projects: null, targets: null, repos: null };

  function loadFormData() {
    var jobs = [];
    if (!formData.projects) jobs.push(api("GET", "/linear/projects").then(function (r) { formData.projects = r.projects || []; }).catch(function () { formData.projects = []; }));
    if (!formData.targets) jobs.push(api("GET", "/harvest/targets").then(function (r) { formData.targets = r.targets || []; }).catch(function () { formData.targets = []; }));
    if (!formData.repos) jobs.push(api("GET", "/github/repos").then(function (r) { formData.repos = r.repos || []; }).catch(function () { formData.repos = []; }));
    return Promise.all(jobs);
  }

  function linearOptions(cur) {
    // A desk binds one Linear project, or a whole team when the client's work
    // spans several of its projects.
    var opts = ['<option value="">— no Linear —</option>'];
    var teams = {};
    (formData.projects || []).forEach(function (p) {
      (p.teams || []).forEach(function (t) { teams[p.account + "|" + t.id] = { account: p.account, team: t }; });
    });
    Object.keys(teams).sort().forEach(function (k) {
      var x = teams[k];
      var v = JSON.stringify({ account: x.account, project_id: "", project_name: "", team_id: x.team.id, team_key: x.team.key });
      var sel = cur && !cur.project_id && cur.team_id === x.team.id;
      opts.push('<option value="' + esc(v) + '"' + (sel ? " selected" : "") + ">Whole team " + esc(x.team.key) + " · " + esc(x.team.name) + " (" + esc(x.account) + ")</option>");
    });
    (formData.projects || []).forEach(function (p) {
      var t = (p.teams || [])[0] || {};
      var v = JSON.stringify({ account: p.account, project_id: p.id, project_name: p.name, team_id: t.id || "", team_key: t.key || "" });
      var sel = cur && cur.project_id === p.id;
      opts.push('<option value="' + esc(v) + '"' + (sel ? " selected" : "") + ">" + esc(t.key || "?") + " · " + esc(p.name) + " — " + esc(p.state || "") + " (" + esc(p.account) + ")</option>");
    });
    return opts.join("");
  }

  function harvestOptions(cur) {
    return '<option value="">— no Harvest default —</option>' + (formData.targets || []).map(function (x) {
      var v = JSON.stringify({ account_id: x.account_id, project_id: x.project_id, task_id: x.task_id, label: targetLabel(x) });
      var sel = cur && cur.project_id === x.project_id && cur.task_id === x.task_id;
      return '<option value="' + esc(v) + '"' + (sel ? " selected" : "") + ">" + esc(targetLabel(x)) + "</option>";
    }).join("");
  }

  function openForm(id) {
    var d = id ? (view.desks || []).filter(function (k) { return k.id === id; })[0] : null;
    formEl.hidden = false;
    formBackdrop.hidden = false;
    formEl.innerHTML = '<div class="card-title">' + (d ? "Edit the desk " + esc(d.name) : "New desk") + '</div><p class="placeholder-note">reading Linear, Harvest and GitHub…</p>';
    loadFormData().then(function () {
      formEl.innerHTML =
        '<div class="card-title"><span>' + (d ? "Edit the desk " + esc(d.name) : "New desk — one per project") + '</span>' +
        '<button class="btn btn-tiny" type="button" data-form-close="1">Close</button></div>' +
        '<div class="desk-form-grid">' +
        '<div><div class="desk-label">Name</div><input id="df-name" class="desk-input" type="text" value="' + esc(d ? d.name : "") + '" placeholder="The client or project" /></div>' +
        '<div><div class="desk-label">Linear — one project, or the whole team</div><select id="df-linear" class="desk-input">' + linearOptions(d && d.linear) + "</select></div>" +
        '<div><div class="desk-label">Harvest project and task (Intercreate)</div><select id="df-harvest" class="desk-input">' + harvestOptions(d && d.harvest) + "</select></div>" +
        '<div><div class="desk-label">Firmware repo folder</div><input id="df-repo" class="desk-input mono" type="text" value="' + esc(d ? (d.firmware_repos || [])[0] || "" : (view.open_desk ? "" : "")) + '" placeholder="/home/you/src/client-fw" /></div>' +
        '<div><div class="desk-label">GitHub repos (owner/repo, one per line)</div><textarea id="df-github" class="desk-input desk-textarea" rows="3" list="df-github-list" spellcheck="false">' + esc(d ? (d.github_repos || []).join("\n") : "") + "</textarea>" +
        '<div class="desk-detail">yours, most recently pushed: ' + esc((formData.repos || []).slice(0, 8).join(", ")) + "</div></div>" +
        '<div><div class="desk-label">Saved bench (Topology) — optional</div><input id="df-bench" class="desk-input" type="text" value="' + esc(d && d.bench ? d.bench : "") + '" placeholder="the saved bench\'s name in that repo" /></div>' +
        "</div>" +
        '<div class="desk-dialog-actions"><span class="desk-detail" id="df-status"></span><span style="flex:1"></span>' +
        (d ? '<button class="btn desk-no-big" type="button" data-form-delete="' + esc(d.id) + '">Delete desk</button>' : "") +
        '<button class="btn btn-primary" type="button" data-form-save="' + esc(d ? d.id : "") + '">' + (d ? "Save" : "Create and open") + "</button></div>";
    });
  }

  function closeForm() {
    formEl.hidden = true;
    formBackdrop.hidden = true;
  }

  function saveForm(id) {
    var status = document.getElementById("df-status");
    var parse = function (sel) { var v = document.getElementById(sel).value; return v ? JSON.parse(v) : null; };
    var repo = document.getElementById("df-repo").value.trim();
    var body = {
      id: id || null,
      name: document.getElementById("df-name").value.trim(),
      linear: parse("df-linear"),
      harvest: parse("df-harvest"),
      firmware_repos: repo ? [repo] : [],
      github_repos: document.getElementById("df-github").value.split(/[\n,]/).map(function (x) { return x.trim(); }).filter(Boolean),
      bench: document.getElementById("df-bench").value.trim() || null,
    };
    status.textContent = "saving…";
    api("POST", "/desks", body).then(function (r) {
      closeForm();
      if (!id) {
        // A new desk opens: that is what making one is for.
        return api("POST", "/open", { desk: r.id }).then(function () {
          document.dispatchEvent(new CustomEvent("embarch:desk-switched"));
        });
      }
    }).then(function () { return load(); }).catch(function (e) { status.textContent = e.message; });
  }

  function render() {
    if (!view) return;
    renderSlackBar();
    renderHead();
    renderElsewhere();
    if (!pickedDay) renderDay(view.day);
    var openId = view.open_desk ? view.open_desk.id : null;
    if (openId !== lastOpenId || lastOpenId === undefined) {
      lastOpenId = openId;
      document.dispatchEvent(new CustomEvent("embarch:desk", { detail: { desk: view.open_desk } }));
    }
    renderTimer();
    renderDrafts();
    var cards = view.cards || [];
    cardsEl.innerHTML = cards.length ? cards.map(cardHtml).join("") :
      '<div class="card desk-empty"><div class="card-title">No work items yet</div>' +
      '<p class="placeholder-note">' + (view.open_desk
        ? "Add a card from a Linear issue key above, or start a Claude Code session in this desk's repos and answer which work item it is."
        : "Open a desk with Switch…, or make one with New desk.") + "</p></div>";
    renderSlack();
    renderSettings();
    if (openTileId) renderDialog();
  }

  function load() {
    return api("GET", "/view").then(function (v) {
      view = v;
      showError(null);
      render();
    }).catch(function (e) {
      showError(e.status === 503 ? e.message : "Could not reach embarch-desk: " + e.message);
    });
  }

  // ---------------------------------------------------------------- the tile dialog

  function findTile(id) {
    var out = null;
    (view.cards || []).forEach(function (c) {
      (c.tiles || []).forEach(function (t) { if (t.id === id) out = { card: c, tile: t }; });
    });
    return out;
  }

  function targetLabel(t) {
    return (t.client_name ? t.client_name + " / " : "") + t.project_name + " / " + t.task_name;
  }

  function renderDialog() {
    var found = findTile(openTileId);
    if (!found) { closeDialog(); return; }
    var t = found.tile, c = found.card;
    var h = t.harvest;
    var options = '<option value="">— pick, or Polish to have Claude guess —</option>';
    if (targets) {
      options += targets.map(function (x) {
        var v = x.account_id + ":" + x.project_id + ":" + x.task_id;
        var sel = h && h.account_id === x.account_id && h.project_id === x.project_id && h.task_id === x.task_id;
        return '<option value="' + v + '"' + (sel ? " selected" : "") + ">" + esc(targetLabel(x)) + "</option>";
      }).join("");
    } else if (h) {
      options += '<option value="' + h.account_id + ":" + h.project_id + ":" + h.task_id + '" selected>' + esc(h.label) + "</option>";
    }
    dialogEl.innerHTML =
      '<div class="card-title"><span>' + esc(c.issue ? c.issue.key + " · " : "") + esc(c.title) + " — " + esc(dayLabel(t.date)) + '</span>' +
      '<button class="btn btn-tiny" type="button" id="desk-dialog-close">Close</button></div>' +
      '<div class="desk-dialog-grid">' +
      "<div><div class=\"desk-label\">Lines</div><ul class=\"desk-lines\">" +
      ((t.lines || []).map(function (l) {
        return '<li><span class="desk-line-src">' + esc(l.source) + "</span> " + esc(l.text) + "</li>";
      }).join("") || '<li class="placeholder-note">No lines yet. Commits land here by themselves; sessions add milestones.</li>') +
      '</ul><div class="desk-form"><input id="desk-line" class="desk-input" type="text" placeholder="Add a line yourself" />' +
      '<button class="btn btn-tiny" type="button" id="desk-line-add">Add</button></div></div>' +
      "<div>" +
      '<div class="desk-label">Hours <span class="desk-detail">(captured ' + hours(t.computed_hours) + ")</span></div>" +
      '<input id="desk-hours" class="desk-input" type="number" min="0" max="24" step="0.25" value="' + esc(t.hours) + '" />' +
      '<div class="desk-label">Harvest project and task</div>' +
      '<select id="desk-target" class="desk-input">' + options + "</select>" +
      '<div class="desk-label">Note</div>' +
      '<textarea id="desk-note" class="desk-input desk-textarea" rows="4" placeholder="Polish drafts this from the lines">' + esc(t.note || "") + "</textarea>" +
      "</div></div>" +
      '<div class="desk-dialog-actions">' +
      '<button class="btn" type="button" id="desk-polish">Polish with Claude</button>' +
      '<span class="desk-detail" id="desk-dialog-status"></span>' +
      '<span style="flex:1"></span>' +
      '<button class="btn" type="button" id="desk-save">Save</button>' +
      '<button class="btn desk-no-big" type="button" id="desk-discard-big">✕ Discard</button>' +
      '<button class="btn btn-primary" type="button" id="desk-approve-big">✓ Add to Harvest</button>' +
      "</div>";
  }

  function openDialog(id) {
    openTileId = id;
    dialogEl.hidden = false;
    backdropEl.hidden = false;
    renderDialog();
    if (!targets) {
      api("GET", "/harvest/targets").then(function (r) {
        targets = r.targets || [];
        if (openTileId === id) renderDialog();
      }).catch(function (e) { setDialogStatus(e.message); });
    }
  }

  function closeDialog() {
    openTileId = null;
    dialogEl.hidden = true;
    backdropEl.hidden = true;
  }

  function setDialogStatus(msg) {
    var el = document.getElementById("desk-dialog-status");
    if (el) el.textContent = msg || "";
  }

  function dialogPatch() {
    var body = {};
    var note = document.getElementById("desk-note");
    var hoursEl = document.getElementById("desk-hours");
    var target = document.getElementById("desk-target");
    if (note) body.note = note.value;
    if (hoursEl && hoursEl.value !== "") body.hours = Number(hoursEl.value);
    if (target && target.value) {
      var p = target.value.split(":").map(Number);
      var opt = target.options[target.selectedIndex];
      body.harvest = { account_id: p[0], project_id: p[1], task_id: p[2], label: opt ? opt.textContent : "" };
    }
    return api("PATCH", "/tiles/" + encodeURIComponent(openTileId), body);
  }

  // ---------------------------------------------------------------- actions

  function approve(id, fromDialog) {
    var found = findTile(id);
    if (!found) return;
    // A tile nobody has looked at has no note and no project: open it rather
    // than send a guess the user never saw.
    if (!fromDialog && !(found.tile.harvest && found.tile.note)) { openDialog(id); return; }
    var step = fromDialog ? dialogPatch() : Promise.resolve();
    step.then(function () {
      return api("POST", "/tiles/" + encodeURIComponent(id) + "/approve");
    }).then(function (r) {
      closeDialog();
      flash("Added to Harvest: " + hours(r.hours) + ".");
      return load();
    }).catch(function (e) {
      if (fromDialog) setDialogStatus(e.message); else flash(e.message);
    });
  }

  function discard(id) {
    if (!window.confirm("Discard this tile? Its time will not reach Harvest.")) return;
    api("POST", "/tiles/" + encodeURIComponent(id) + "/discard").then(function () {
      closeDialog();
      return load();
    }).catch(function (e) { flash(e.message); });
  }

  function pickFolder(itemId) {
    var c = (view.cards || []).filter(function (x) { return x.id === itemId; })[0];
    var p = window.prompt("The work item's repo folder on this machine (absolute path):", (c && c.repo_path) || "");
    if (!p) return Promise.reject(new Error("no folder picked"));
    return api("POST", "/items/" + encodeURIComponent(itemId) + "/link", { repo_path: p.trim() });
  }

  function launch(itemId) {
    api("POST", "/items/" + encodeURIComponent(itemId) + "/launch").catch(function (e) {
      if (e.status !== 409) throw e;
      return pickFolder(itemId).then(function () {
        return api("POST", "/items/" + encodeURIComponent(itemId) + "/launch");
      });
    }).then(function (r) {
      // The folder first; the Claude panel opens in whichever window has
      // focus, so give the new window time to take it.
      window.location.href = r.folder_uri;
      setTimeout(function () { window.location.href = r.claude_uri; }, 4000);
      flash("Opening " + r.repo + " — the first Claude Code session there in the next 10 minutes lands on this card.");
      load();
    }).catch(function (e) { if (e.message !== "no folder picked") flash(e.message); });
  }

  panel.addEventListener("click", function (ev) {
    var t = ev.target.closest("button, a");
    if (!t || !panel.contains(t)) return;
    var d = t.dataset;
    if (d.openTile) { openDialog(d.openTile); return; }
    if (d.timerStart) { api("POST", "/timer/start", { item_id: d.timerStart }).then(load).catch(function (e) { flash(e.message); }); return; }
    if (d.timerStop) { api("POST", "/timer/stop").then(load).catch(function (e) { flash(e.message); }); return; }
    if (d.draftSend || d.draftDiscard) {
      var id = d.draftSend || d.draftDiscard;
      var box = draftsEl.querySelector('[data-draft-id="' + CSS.escape(id) + '"]');
      if (d.draftDiscard) {
        api("POST", "/drafts/" + encodeURIComponent(id) + "/discard").then(load).catch(function (e) { flash(e.message); });
        return;
      }
      var patch = { body: box.querySelector(".desk-draft-body").value };
      var titleEl = box.querySelector(".desk-draft-title");
      if (titleEl) patch.title = titleEl.value;
      t.disabled = true;
      api("PATCH", "/drafts/" + encodeURIComponent(id), patch).then(function () {
        return api("POST", "/drafts/" + encodeURIComponent(id) + "/send");
      }).then(function (r) { flash("Sent: " + (r.url || "done")); return load(); })
        .catch(function (e) { flash(e.message); t.disabled = false; });
      return;
    }
    if (d.approve) { approve(d.approve, false); return; }
    if (d.discard) { discard(d.discard); return; }
    if (d.launch) { launch(d.launch); return; }
    if (d.folder) { pickFolder(d.folder).then(load).catch(function (e) { if (e.message !== "no folder picked") flash(e.message); }); return; }
    if (d.pin) {
      api("POST", "/items/" + encodeURIComponent(d.pin) + "/pin", { value: d.value === "true" }).then(load).catch(function (e) { flash(e.message); });
      return;
    }
    if (d.archive) {
      if (!window.confirm("Archive this card? It leaves the desk; open tiles on it stay unsent.")) return;
      api("POST", "/items/" + encodeURIComponent(d.archive) + "/archive", { value: true }).then(load).catch(function (e) { flash(e.message); });
      return;
    }
    if (d.link) {
      var key = window.prompt("Linear issue key for this card (e.g. ABC-123):");
      if (!key) return;
      api("POST", "/items/" + encodeURIComponent(d.link) + "/link", { issue_key: key.trim() }).then(load).catch(function (e) { flash(e.message); });
      return;
    }
    if (d.disconnect) {
      var parts = d.disconnect.split("/");
      if (!window.confirm("Remove the " + parts[0] + " connector " + parts.slice(1).join("/") + "?")) return;
      api("DELETE", "/connectors/" + encodeURIComponent(parts[0]) + "/" + encodeURIComponent(parts.slice(1).join("/"))).then(load).catch(function (e) { flash(e.message); });
      return;
    }
    if (d.draft !== undefined) {
      var m = view.slack.items[Number(d.draft)];
      var box = slackEl.querySelector('[data-reply-box="' + d.draft + '"]');
      box.hidden = false;
      box.innerHTML = '<p class="placeholder-note">Claude is drafting…</p>';
      api("POST", "/slack/draft", { account: m.account, channel: m.channel, ts: m.ts }).then(function (r) {
        box.innerHTML = '<textarea class="desk-input desk-textarea" rows="3">' + esc(r.draft) + "</textarea>" +
          '<div class="desk-form"><button class="btn btn-primary btn-tiny" type="button" data-send="' + d.draft + '" data-thread="' + esc(r.thread_ts) + '">Send as me</button>' +
          '<button class="btn btn-tiny" type="button" data-cancel-reply="' + d.draft + '">Cancel</button></div>';
      }).catch(function (e) { box.innerHTML = '<p class="placeholder-note" style="color:var(--danger);">' + esc(e.message) + "</p>"; });
      return;
    }
    if (d.send !== undefined) {
      var msg = view.slack.items[Number(d.send)];
      var b2 = slackEl.querySelector('[data-reply-box="' + d.send + '"]');
      var text = b2.querySelector("textarea").value;
      api("POST", "/slack/reply", { account: msg.account, channel: msg.channel, thread_ts: d.thread || null, text: text }).then(function () {
        b2.innerHTML = '<p class="placeholder-note" style="color:var(--success);">Sent.</p>';
      }).catch(function (e) { flash(e.message); });
      return;
    }
    if (d.cancelReply !== undefined) {
      var b3 = slackEl.querySelector('[data-reply-box="' + d.cancelReply + '"]');
      b3.hidden = true;
      b3.innerHTML = "";
      return;
    }
    if (d.entryEdit) {
      var e = findEntry(d.entryEdit);
      var li = t.closest(".desk-entry");
      if (!e || !li) return;
      li.innerHTML = '<input class="desk-input desk-entry-h" type="number" min="0" max="24" step="0.05" value="' + esc(e.hours) + '" />' +
        '<textarea class="desk-input desk-textarea desk-entry-n" rows="2">' + esc(e.notes) + "</textarea>" +
        '<button class="btn btn-tiny btn-primary" type="button" data-entry-save="' + esc(d.entryEdit) + '">Save to Harvest</button>' +
        '<button class="btn btn-tiny" type="button" data-entry-cancel="1">Cancel</button>';
      return;
    }
    if (d.entrySave) {
      var row = t.closest(".desk-entry");
      var parts = d.entrySave.split("/");
      t.disabled = true;
      api("PATCH", "/harvest/entry/" + parts[0] + "/" + parts[1], {
        hours: Number(row.querySelector(".desk-entry-h").value),
        notes: row.querySelector(".desk-entry-n").value,
      }).then(function () { return refreshDay(); }).catch(function (err) { flash(err.message); t.disabled = false; });
      return;
    }
    if (d.entryCancel) { renderDay(lastDay); return; }
    if (d.entryDelete) {
      var en = findEntry(d.entryDelete);
      if (!window.confirm("Delete this entry from Harvest" + (en ? " (" + hours(en.hours) + ", " + en.notes.slice(0, 60) + ")" : "") + "?")) return;
      var p2 = d.entryDelete.split("/");
      api("DELETE", "/harvest/entry/" + p2[0] + "/" + p2[1]).then(function () { return refreshDay(); }).catch(function (err) { flash(err.message); });
      return;
    }
    if (d.formClose) { closeForm(); return; }
    if (d.formSave !== undefined) { saveForm(d.formSave); return; }
    if (d.formDelete) {
      if (!window.confirm("Delete this desk? Its work items keep their time and become deskless.")) return;
      api("DELETE", "/desks/" + encodeURIComponent(d.formDelete)).then(function () { closeForm(); return load(); }).catch(function (e) { flash(e.message); });
      return;
    }
    switch (t.id) {
      case "desk-switch":
        document.getElementById("project-button").click();
        break;
      case "desk-create":
        openForm(null);
        break;
      case "desk-edit":
        if (view.open_desk) openForm(view.open_desk.id);
        break;
      case "desk-slack-toggle":
        slackEl.hidden = !slackEl.hidden;
        t.setAttribute("aria-expanded", String(!slackEl.hidden));
        t.querySelector(".desk-caret").textContent = slackEl.hidden ? "▸" : "▾";
        renderSlack();
        break;
      case "desk-settings-toggle":
        settingsEl.hidden = !settingsEl.hidden;
        renderSettings();
        break;
      case "desk-refresh":
        t.disabled = true;
        api("POST", "/sync").then(load).catch(function (e) { flash(e.message); }).then(function () { t.disabled = false; });
        break;
      case "desk-add": {
        var input = document.getElementById("desk-new");
        var v = input.value.trim();
        if (!v) break;
        var body = /^[A-Za-z]{1,10}-\d+$/.test(v) ? { issue_key: v } : { title: v };
        if (!body.issue_key) {
          if ((view.desks || []).length && !newDeskSel.value) { flash("Pick the standalone card's desk first."); break; }
          body.desk = newDeskSel.value || undefined;
        }
        api("POST", "/items", body).then(function () { input.value = ""; return load(); }).catch(function (e) { flash(e.message); });
        break;
      }
      case "desk-conn-add": {
        var kind = document.getElementById("desk-conn-kind").value;
        var label = document.getElementById("desk-conn-label").value.trim();
        var token = document.getElementById("desk-conn-token").value.trim();
        if (!label || !token) { flash("A connector needs a name and a token."); break; }
        t.disabled = true;
        api("POST", "/connectors", { kind: kind, label: label, token: token }).then(function (r) {
          flash("Connected " + kind + " / " + label + ": " + (r.detail || "ok"));
          return api("POST", "/sync");
        }).then(load).catch(function (e) { flash(e.message); }).then(function () { t.disabled = false; });
        break;
      }
      case "desk-ignore-save": {
        var paths = document.getElementById("desk-ignore").value.split("\n").map(function (s) { return s.trim(); }).filter(Boolean);
        api("PUT", "/ignore", { paths: paths }).then(load).catch(function (e) { flash(e.message); });
        break;
      }
    }
  });

  panel.addEventListener("focusin", function (ev) {
    var sel = ev.target.closest("select[data-status]");
    if (!sel || states[sel.dataset.status]) return;
    var id = sel.dataset.status;
    api("GET", "/items/" + encodeURIComponent(id) + "/states").then(function (r) {
      states[id] = r.states || [];
      var cur = sel.value;
      sel.innerHTML = states[id].map(function (s) {
        return '<option value="' + esc(s.id) + '"' + (s.id === cur ? " selected" : "") + ">" + esc(s.name) + "</option>";
      }).join("");
    }).catch(function (e) { flash(e.message); });
  });

  panel.addEventListener("change", function (ev) {
    var sel = ev.target.closest("select[data-status]");
    if (!sel) return;
    api("POST", "/items/" + encodeURIComponent(sel.dataset.status) + "/status", { state_id: sel.value })
      .then(load).catch(function (e) { flash(e.message); load(); });
  });

  dialogEl.addEventListener("click", function (ev) {
    var t = ev.target.closest("button");
    if (!t) return;
    var id = openTileId;
    switch (t.id) {
      case "desk-dialog-close": closeDialog(); break;
      case "desk-save":
        dialogPatch().then(function () { setDialogStatus("Saved."); return load(); }).catch(function (e) { setDialogStatus(e.message); });
        break;
      case "desk-polish":
        t.disabled = true;
        setDialogStatus("Claude is drafting the note and picking a project…");
        dialogPatch().then(function () {
          return api("POST", "/tiles/" + encodeURIComponent(id) + "/polish");
        }).then(function () { return load(); }).then(function () { setDialogStatus("Drafted — edit it before you send."); })
          .catch(function (e) { setDialogStatus(e.message); }).then(function () { t.disabled = false; });
        break;
      case "desk-line-add": {
        var inp = document.getElementById("desk-line");
        if (!inp.value.trim()) break;
        var text = inp.value.trim();
        dialogPatch().then(function () {
          return api("POST", "/tiles/" + encodeURIComponent(id) + "/line", { text: text });
        }).then(load).catch(function (e) { setDialogStatus(e.message); });
        break;
      }
      case "desk-approve-big": approve(id, true); break;
      case "desk-discard-big": discard(id); break;
    }
  });

  backdropEl.addEventListener("click", closeDialog);
  formBackdrop.addEventListener("click", closeForm);
  dayDate.addEventListener("change", function () { loadDay(dayDate.value); });
  document.addEventListener("embarch:desk-form", function (ev) { openForm(ev.detail ? ev.detail.id : null); });
  // A switch made from the shell: refresh at once rather than on the next push.
  document.addEventListener("embarch:desk-switched", function () { lastOpenId = undefined; load(); });
  document.addEventListener("keydown", function (ev) { if (ev.key === "Escape" && openTileId) closeDialog(); });

  // Live by SSE, like every other tab (decision 6): the server asks the desk
  // for its view and pushes it. A pushed view never re-renders under an open
  // editor — the dialog, a reply box and the connector form keep what the user
  // is typing until they finish.
  function editing() {
    return dialogEl.contains(document.activeElement) || slackEl.contains(document.activeElement) ||
      settingsEl.contains(document.activeElement) || draftsEl.contains(document.activeElement) ||
      formEl.contains(document.activeElement) || dayBody.contains(document.activeElement);
  }
  if (window.EventSource) {
    var source = new EventSource("/api/desk-events");
    source.addEventListener("view", function (ev) {
      try { view = JSON.parse(ev.data); } catch (_) { return; }
      showError(null);
      if (!editing()) render();
    });
    source.addEventListener("desk-error", function (ev) { showError(ev.data); });
  }
  document.querySelectorAll('.nav-item[data-tab="desk"]').forEach(function (el) { el.addEventListener("click", load); });
  load();
})();
