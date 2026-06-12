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
<title>Lookout — API Keys</title>
<style>
  :root { color-scheme: light dark; }
  * { box-sizing: border-box; }
  body {
    font: 15px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
    margin: 0; padding: 2rem; max-width: 920px; margin-inline: auto;
  }
  h1 { font-size: 1.4rem; margin: 0 0 1.5rem; }
  form { display: flex; gap: .5rem; margin-bottom: 1.5rem; }
  input[type=text] {
    flex: 1; padding: .55rem .7rem; border: 1px solid #8884; border-radius: 6px;
    font: inherit; background: transparent; color: inherit;
  }
  button {
    padding: .55rem .9rem; border: 1px solid #8884; border-radius: 6px;
    background: #2563eb; color: #fff; font: inherit; cursor: pointer;
  }
  button.secondary { background: transparent; color: inherit; }
  button.danger { background: transparent; color: #dc2626; border-color: #dc26264d; }
  button:hover { filter: brightness(1.08); }
  table { width: 100%; border-collapse: collapse; }
  th, td { text-align: left; padding: .6rem .5rem; border-bottom: 1px solid #8882; }
  th { font-size: .8rem; text-transform: uppercase; letter-spacing: .04em; opacity: .6; }
  code {
    font: 13px/1.4 ui-monospace, SFMono-Regular, Menlo, monospace;
    background: #8881; padding: .15rem .4rem; border-radius: 4px;
  }
  .muted { opacity: .55; font-size: .85rem; }
  .num { font-variant-numeric: tabular-nums; }
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
  .row-actions { display: flex; gap: .4rem; justify-content: flex-end; }
  #msg { margin-bottom: 1rem; min-height: 1.2rem; color: #16a34a; }
  #msg.error { color: #dc2626; }
  .empty { text-align: center; padding: 2rem; opacity: .5; }
</style>
</head>
<body>
  <h1>Lookout — Program API Keys</h1>
  <div id="msg"></div>
  <div id="stats" class="stats"></div>
  <form id="create-form">
    <input type="text" id="name" placeholder="Program name (e.g. arcade)" required
           autocomplete="off" maxlength="255" />
    <button type="submit">Create key</button>
  </form>
  <div id="legend" class="legend"></div>
  <table>
    <thead>
      <tr><th>Program</th><th>Key</th><th>Sessions</th><th>Hours</th><th>Progress</th><th>Last used</th><th>Created</th><th></th></tr>
    </thead>
    <tbody id="rows"></tbody>
  </table>

<script>
var rows = document.getElementById("rows");
var msg = document.getElementById("msg");

// Session statuses in lifecycle order, each with a color. Mirrors the
// session_status pg enum; drives both the Progress column and the legend.
var STATUS_META = [
  ["pending", "#9ca3af"],
  ["active", "#2563eb"],
  ["paused", "#d97706"],
  ["stopped", "#64748b"],
  ["compiling", "#7c3aed"],
  ["complete", "#16a34a"],
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

function rowHtml(k) {
  return "<tr>" +
    "<td>" + esc(k.name) + "</td>" +
    "<td><code>" + esc(k.key) + "</code> " +
      '<button class="secondary" data-copy="' + esc(k.key) + '">copy</button></td>' +
    '<td class="num">' + (k.sessionCount || 0) + "</td>" +
    "<td>" + hours(k.trackedSeconds) + "</td>" +
    "<td>" + progress(k.statusCounts) + "</td>" +
    "<td>" + fmt(k.lastUsedAt) + "</td>" +
    "<td>" + fmt(k.createdAt) + "</td>" +
    '<td class="row-actions">' +
      '<button class="danger" data-del="' + esc(k.id) + '" data-name="' + esc(k.name) +
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
    var result = await api("GET", "/api/admin/keys");
    var keys = result.keys || [];
    renderStats(result.totals);
    if (!keys.length) {
      rows.innerHTML = '<tr><td colspan="8" class="empty">No keys yet.</td></tr>';
      return;
    }
    rows.innerHTML = keys.map(rowHtml).join("");
  } catch (e) {
    flash(e.message, true);
  }
}

document.getElementById("create-form").addEventListener("submit", async function (ev) {
  ev.preventDefault();
  var input = document.getElementById("name");
  var name = input.value.trim();
  if (!name) return;
  try {
    var created = await api("POST", "/api/admin/keys", { name: name });
    input.value = "";
    flash('Created key for "' + created.name + '".');
    await load();
  } catch (e) {
    flash(e.message, true);
  }
});

rows.addEventListener("click", async function (ev) {
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
  var delBtn = ev.target.closest("[data-del]");
  if (delBtn) {
    var name = delBtn.getAttribute("data-name");
    if (!confirm('Delete the key for "' + name + '"? This cannot be undone.')) return;
    try {
      await api("DELETE", "/api/admin/keys/" + delBtn.getAttribute("data-del"));
      flash('Deleted key for "' + name + '".');
      await load();
    } catch (e) {
      flash(e.message, true);
    }
  }
});

load();
</script>
</body>
</html>`;
