// Self-contained admin dashboard page. Exported as a template literal so `tsc`
// ships it in dist with no asset-copy step. Served by GET /admin behind basic
// auth; the browser auto-attaches credentials to the /api/admin/* fetches.
//
// The embedded <script> deliberately uses string concatenation (no backticks,
// no ${} interpolation) so the whole page can live in one String.raw literal
// without escaping headaches.
export const ADMIN_PAGE_HTML = String.raw`<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>Lookout — Programs</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body {
    font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    margin: 0; padding: 2rem; max-width: 1400px; margin-inline: auto;
  }
  h1 { font-size: 1.4rem; margin: 0 0 1.5rem; }
  form { display: flex; gap: .5rem; margin-bottom: 1.5rem; flex-wrap: wrap; }
  input[type=text], input[type=url], select {
    padding: .55rem .7rem; border: 1px solid #8884; border-radius: 6px;
    font: inherit; background: transparent; color: inherit;
  }
  h2.section { font-size: 1rem; margin: 0 0 .75rem; opacity: .7; text-transform: uppercase; letter-spacing: .04em; }
  input#ann-message { flex: 3; min-width: 16rem; }
  input#ann-url { flex: 2; min-width: 14rem; }
  /* Current-announcement preview, colored by level via inline border/bg. */
  .ann-current {
    display: flex; align-items: center; gap: .6rem; padding: .6rem .8rem;
    border: 1px solid #8884; border-radius: 8px; margin-bottom: .75rem;
  }
  .ann-current .badge {
    font-size: .72rem; text-transform: uppercase; letter-spacing: .04em;
    font-weight: 600; padding: .1rem .45rem; border-radius: 999px; color: #fff;
  }
  .ann-current .ann-msg { flex: 1; min-width: 0; }
  .ann-current .ann-link { font-size: .82rem; opacity: .8; word-break: break-all; }
  input#name { flex: 1; min-width: 12rem; }
  input#display-name { flex: 1; min-width: 12rem; }
  input#url { flex: 2; min-width: 16rem; }
  button {
    padding: .5rem .8rem; border: 1px solid #8884; border-radius: 6px;
    background: #2563eb; color: #fff; font: inherit; cursor: pointer;
    white-space: nowrap;
  }
  button.secondary { background: transparent; color: inherit; padding: .35rem .6rem; font-size: .85rem; }
  button.danger { background: transparent; color: #dc2626; border-color: #dc26264d; }
  button:hover { filter: brightness(1.08); }
  /* Content-sized columns; only the URL and key cells wrap (everything else
     stays on its natural line so labels/timestamps/counts don't fracture). */
  table { width: 100%; border-collapse: collapse; }
  th, td {
    text-align: left; padding: .6rem .5rem; border-bottom: 1px solid #8882;
    vertical-align: top;
  }
  th { font-size: .8rem; text-transform: uppercase; letter-spacing: .04em; opacity: .6; white-space: nowrap; }
  code {
    font: 13px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace;
    background: #8881; padding: .15rem .4rem; border-radius: 4px;
  }
  .muted { opacity: .55; font-size: .85rem; }
  .url { font-size: .82rem; display: inline-block; max-width: 15rem; word-break: break-all; }
  .num { font-variant-numeric: tabular-nums; white-space: nowrap; }
  .keylist { display: flex; flex-direction: column; gap: .35rem; }
  .keyrow { display: flex; gap: .35rem; align-items: center; }
  /* Masked key stays on one line; only the revealed (long) key wraps. */
  .keyrow code.key { display: inline-block; white-space: nowrap; }
  .keyrow code.key[data-shown="1"] { white-space: normal; word-break: break-all; max-width: 30ch; }
  .keyrow button { flex-shrink: 0; }
  .stats {
    display: grid; grid-template-columns: repeat(3, 1fr); gap: 1px;
    background: #8882; border: 1px solid #8882; border-radius: 8px;
    overflow: hidden; margin-bottom: 1.5rem;
  }
  .stat { background: Canvas; padding: .9rem 1.1rem; }
  .stat .label { font-size: .72rem; text-transform: uppercase; letter-spacing: .04em; opacity: .55; }
  .stat .value { font-size: 1.5rem; font-weight: 600; margin-top: .2rem; font-variant-numeric: tabular-nums; }
  .stat .value.prog { font-size: 1.3rem; }
  .prog { font-variant-numeric: tabular-nums; white-space: nowrap; font-weight: 600; }
  .prog .sep, .legend .sep { opacity: .3; margin: 0 1px; font-weight: 400; }
  .legend { font-size: .8rem; margin-bottom: 1.25rem; }
  .legend .sep { margin: 0 4px; }
  .row-actions { display: flex; gap: .4rem; justify-content: flex-end; flex-wrap: wrap; }
  #msg { margin-bottom: 1rem; min-height: 1.2rem; color: #16a34a; }
  #msg.error { color: #dc2626; }
  .empty { text-align: center; padding: 2rem; opacity: .5; }
</style>
</head>
<body>
  <h1>Lookout — Programs</h1>
  <div id="msg"></div>
  <div id="stats" class="stats"></div>

  <h2 class="section">Announcement</h2>
  <div id="announcement-current"></div>
  <form id="announcement-form">
    <select id="ann-level" aria-label="Level">
      <option value="info">Info</option>
      <option value="success">Success</option>
      <option value="warning">Warning</option>
      <option value="danger">Danger</option>
    </select>
    <input type="text" id="ann-message" placeholder="Message (shown in the desktop app)"
           autocomplete="off" maxlength="500" />
    <input type="url" id="ann-url" placeholder="URL (optional, opens in browser)"
           autocomplete="off" maxlength="2048" />
    <button type="submit">Set announcement</button>
    <button type="button" id="ann-clear" class="danger">Clear</button>
  </form>

  <h2 class="section">Programs</h2>
  <form id="create-form">
    <input type="text" id="name" placeholder="Program name (e.g. arcade)" required
           autocomplete="off" maxlength="255" />
    <input type="text" id="display-name" placeholder="Display name (optional, e.g. Arcade)"
           autocomplete="off" maxlength="255" />
    <input type="url" id="url" placeholder="New-session URL (optional, e.g. https://arcade.hackclub.com/lookout_session/new?desktop=true)"
           autocomplete="off" maxlength="2048" />
    <button type="submit">Create program</button>
  </form>
  <div id="legend" class="legend"></div>
  <table>
    <thead>
      <tr><th>Program</th><th>New-session URL</th><th>Key(s)</th><th>Sessions</th><th>Hours</th><th>Progress</th><th>Last used</th><th>Created</th><th></th></tr>
    </thead>
    <tbody id="rows"></tbody>
  </table>

<script>
var rows = document.getElementById("rows");
var msg = document.getElementById("msg");
var COLSPAN = 9;

// Session statuses in lifecycle order, each with a color. Mirrors the
// session_status pg enum; drives both the Progress column and the legend.
var STATUS_META = [
  ["pending", "#9ca3af"],
  ["active", "#2563eb"],
  ["paused", "#d97706"],
  ["stopped", "#64748b"],
  ["compiling", "#7c3aed"],
  ["complete", "#16a34a"],
  ["empty", "#a8a29e"],
  ["failed", "#dc2626"],
];

document.getElementById("legend").innerHTML =
  '<span class="muted">Progress: </span>' +
  STATUS_META.map(function (m) {
    return '<span style="color:' + m[1] + '">' + m[0] + "</span>";
  }).join('<span class="sep">/</span>');

function flash(text, isError) {
  msg.textContent = text;
  msg.className = isError ? "error" : "";
  if (text) setTimeout(function () {
    if (msg.textContent === text) msg.textContent = "";
  }, 4000);
}

function fmt(ts) {
  if (!ts) return '<span class="muted">never</span>';
  return new Date(ts).toLocaleString();
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, function (c) {
    return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
  });
}

async function api(method, path, body) {
  var res = await fetch(path, {
    method: method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  var data = await res.json().catch(function () { return {}; });
  if (!res.ok) throw new Error(data.error || ("HTTP " + res.status));
  return data;
}

function hours(sec) {
  return '<span class="num">' + ((sec || 0) / 3600).toFixed(1) + "</span>";
}

// CI-style compacted, color-coded per-status counts, e.g. 12/24/42/44.
function progress(counts) {
  counts = counts || {};
  return '<span class="prog">' + STATUS_META.map(function (m) {
    return '<span style="color:' + m[1] + '" title="' + m[0] + '">' +
      (counts[m[0]] || 0) + "</span>";
  }).join('<span class="sep">/</span>') + "</span>";
}

// Keys are masked by default so they aren't accidentally exposed (e.g. while
// screen-sharing the dashboard); "reveal" toggles the plaintext on demand.
var KEY_MASK = "lk_••••••••";
function keysHtml(keys) {
  if (!keys || !keys.length) return '<span class="muted">no key</span>';
  return '<div class="keylist">' + keys.map(function (k) {
    return '<div class="keyrow">' +
      '<code class="key" data-full="' + esc(k.key) + '">' + KEY_MASK + "</code>" +
      '<button class="secondary" data-reveal>reveal</button>' +
      '<button class="secondary" data-copy="' + esc(k.key) + '">copy</button>' +
      "</div>";
  }).join("") + "</div>";
}

function urlHtml(u) {
  if (!u) return '<span class="muted">not set</span>';
  return '<span class="url">' + esc(u) + "</span>";
}

function rowHtml(p) {
  // Most recent key activity / earliest key for the "last used" column.
  var lastUsed = (p.keys || []).reduce(function (acc, k) {
    if (k.lastUsedAt && (!acc || new Date(k.lastUsedAt) > new Date(acc))) return k.lastUsedAt;
    return acc;
  }, null);
  // Show the display name prominently (what users see) with the raw name as a
  // muted subtitle, since attribution and the API key key off the raw name.
  var nameCell = p.displayName
    ? esc(p.displayName) + '<br><span class="muted">' + esc(p.name) + "</span>"
    : esc(p.name);
  return "<tr>" +
    "<td>" + nameCell + "</td>" +
    "<td>" + urlHtml(p.newSessionUrl) + "</td>" +
    "<td>" + keysHtml(p.keys) + "</td>" +
    '<td class="num">' + (p.sessionCount || 0) + "</td>" +
    "<td>" + hours(p.trackedSeconds) + "</td>" +
    "<td>" + progress(p.statusCounts) + "</td>" +
    "<td>" + fmt(lastUsed) + "</td>" +
    "<td>" + fmt(p.createdAt) + "</td>" +
    '<td class="row-actions">' +
      '<button class="secondary" data-name-edit="' + esc(p.id) + '" data-name="' + esc(p.name) +
        '" data-current="' + esc(p.displayName || "") + '">set name</button>' +
      '<button class="secondary" data-url="' + esc(p.id) + '" data-name="' + esc(p.name) +
        '" data-current="' + esc(p.newSessionUrl || "") + '">set URL</button>' +
      '<button class="danger" data-del="' + esc(p.id) + '" data-name="' + esc(p.name) +
        '">delete</button></td>' +
    "</tr>";
}

function renderStats(totals) {
  totals = totals || { statusCounts: {} };
  var stats = document.getElementById("stats");
  stats.innerHTML =
    '<div class="stat"><div class="label">Total sessions</div>' +
      '<div class="value">' + (totals.sessionCount || 0) + "</div></div>" +
    '<div class="stat"><div class="label">Cumulative hours</div>' +
      '<div class="value">' + ((totals.trackedSeconds || 0) / 3600).toFixed(1) + "</div></div>" +
    '<div class="stat"><div class="label">Session progress</div>' +
      '<div class="value prog">' + progress(totals.statusCounts) + "</div></div>";
}

async function load() {
  try {
    var result = await api("GET", "/api/admin/programs");
    var programs = result.programs || [];
    renderStats(result.totals);
    if (!programs.length) {
      rows.innerHTML = '<tr><td colspan="' + COLSPAN + '" class="empty">No programs yet.</td></tr>';
      return;
    }
    rows.innerHTML = programs.map(rowHtml).join("");
  } catch (e) {
    flash(e.message, true);
  }
}

document.getElementById("create-form").addEventListener("submit", async function (ev) {
  ev.preventDefault();
  var nameInput = document.getElementById("name");
  var displayNameInput = document.getElementById("display-name");
  var urlInput = document.getElementById("url");
  var name = nameInput.value.trim();
  var displayName = displayNameInput.value.trim();
  var url = urlInput.value.trim();
  if (!name) return;
  try {
    var body = { name: name };
    if (displayName) body.displayName = displayName;
    if (url) body.newSessionUrl = url;
    var created = await api("POST", "/api/admin/programs", body);
    nameInput.value = "";
    displayNameInput.value = "";
    urlInput.value = "";
    flash('Created program "' + created.name + '".');
    await load();
  } catch (e) {
    flash(e.message, true);
  }
});

// ── Announcement banner ─────────────────────────────────────
// Level → color, mirroring the desktop theme's status palette.
var ANN_COLORS = {
  info: "#3b82f6",
  success: "#22c55e",
  warning: "#f59e0b",
  danger: "#ef4444",
};

function renderAnnouncement(a) {
  var box = document.getElementById("announcement-current");
  if (!a) {
    box.innerHTML = '<div class="muted" style="margin-bottom:.75rem">No announcement set.</div>';
    return;
  }
  var color = ANN_COLORS[a.level] || ANN_COLORS.info;
  box.innerHTML =
    '<div class="ann-current" style="border-color:' + color + ';background:' + color + '1a">' +
      '<span class="badge" style="background:' + color + '">' + esc(a.level) + "</span>" +
      '<span class="ann-msg">' + esc(a.message) +
        (a.url ? ' <span class="ann-link">(' + esc(a.url) + ")</span>" : "") +
      "</span></div>";
}

async function loadAnnouncement() {
  try {
    var data = await api("GET", "/api/admin/announcement");
    var a = data.announcement;
    renderAnnouncement(a);
    // Prefill the form with the current values for easy edits.
    if (a) {
      document.getElementById("ann-level").value = a.level;
      document.getElementById("ann-message").value = a.message;
      document.getElementById("ann-url").value = a.url || "";
    }
  } catch (e) {
    flash(e.message, true);
  }
}

document.getElementById("announcement-form").addEventListener("submit", async function (ev) {
  ev.preventDefault();
  var message = document.getElementById("ann-message").value.trim();
  var url = document.getElementById("ann-url").value.trim();
  if (!message) return;
  try {
    var body = { level: document.getElementById("ann-level").value, message: message };
    if (url) body.url = url;
    await api("POST", "/api/admin/announcement", body);
    flash("Announcement set.");
    await loadAnnouncement();
  } catch (e) {
    flash(e.message, true);
  }
});

document.getElementById("ann-clear").addEventListener("click", async function () {
  if (!confirm("Clear the current announcement?")) return;
  try {
    await api("DELETE", "/api/admin/announcement");
    document.getElementById("ann-message").value = "";
    document.getElementById("ann-url").value = "";
    flash("Announcement cleared.");
    await loadAnnouncement();
  } catch (e) {
    flash(e.message, true);
  }
});

rows.addEventListener("click", async function (ev) {
  var revealBtn = ev.target.closest("[data-reveal]");
  if (revealBtn) {
    var code = revealBtn.parentElement.querySelector("code.key");
    if (code.getAttribute("data-shown") === "1") {
      code.textContent = KEY_MASK;
      code.removeAttribute("data-shown");
      revealBtn.textContent = "reveal";
    } else {
      code.textContent = code.getAttribute("data-full");
      code.setAttribute("data-shown", "1");
      revealBtn.textContent = "hide";
    }
    return;
  }

  var copyBtn = ev.target.closest("[data-copy]");
  if (copyBtn) {
    try {
      await navigator.clipboard.writeText(copyBtn.getAttribute("data-copy"));
      flash("Copied to clipboard.");
    } catch (e) {
      flash("Copy failed.", true);
    }
    return;
  }

  var nameEditBtn = ev.target.closest("[data-name-edit]");
  if (nameEditBtn) {
    var currentName = nameEditBtn.getAttribute("data-current");
    var nextName = prompt(
      'Display name for "' + nameEditBtn.getAttribute("data-name") +
        '" (leave blank to clear — falls back to the raw name):',
      currentName,
    );
    if (nextName === null) return; // cancelled
    try {
      await api("PATCH", "/api/admin/programs/" + nameEditBtn.getAttribute("data-name-edit"), {
        displayName: nextName.trim(),
      });
      flash("Updated display name.");
      await load();
    } catch (e) {
      flash(e.message, true);
    }
    return;
  }

  var urlBtn = ev.target.closest("[data-url]");
  if (urlBtn) {
    var current = urlBtn.getAttribute("data-current");
    var next = prompt(
      'New-session URL for "' + urlBtn.getAttribute("data-name") +
        '" (leave blank to clear):',
      current,
    );
    if (next === null) return; // cancelled
    try {
      await api("PATCH", "/api/admin/programs/" + urlBtn.getAttribute("data-url"), {
        newSessionUrl: next.trim(),
      });
      flash("Updated new-session URL.");
      await load();
    } catch (e) {
      flash(e.message, true);
    }
    return;
  }

  var delBtn = ev.target.closest("[data-del]");
  if (delBtn) {
    var name = delBtn.getAttribute("data-name");
    if (!confirm('Delete program "' + name + '" and its key(s)? This cannot be undone.')) return;
    try {
      await api("DELETE", "/api/admin/programs/" + delBtn.getAttribute("data-del"));
      flash('Deleted program "' + name + '".');
      await load();
    } catch (e) {
      flash(e.message, true);
    }
  }
});

load();
loadAnnouncement();
</script>
</body>
</html>`;
