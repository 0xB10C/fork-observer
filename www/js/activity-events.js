// Shared by the activity list (activity.js) and the playback page
// (playback.js): how each event kind of api/<network>/activity.json is
// labelled, described and undone, plus the few page helpers both need.

const SEARCH_PARAM_NETWORK = "network"
const PAGE_NAME = "fork-observer"
// How many events the API serves per request. It is not a parameter of the
// request; this mirrors the server's fixed page size, and is only used to tell
// a full page (there may be more) from a short one (there is nothing older).
const PAGE_SIZE = 100

async function fetch_json(url) {
  let response = await fetch(url)
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`)
  return await response.json()
}

// The network the URL asks for, by friendly slug or - like the tree page - by
// numeric id, falling back to the first one.
function network_from_url(networks) {
  let param = new URLSearchParams(window.location.search).get(SEARCH_PARAM_NETWORK)
  return networks.find(n => n.slug == param || n.id == param) || networks[0]
}

// How long ago a unix timestamp was, as "5m ago". blocktree.js has its own
// ago() for the node table, hence the name.
function relative_time(timestamp) {
  const rtf = new Intl.RelativeTimeFormat("en", { style: "narrow", numeric: "always" })
  const seconds = Math.round(timestamp - Date.now() / 1000)
  if (seconds > -90) return rtf.format(seconds, "seconds")
  const minutes = Math.round(seconds / 60)
  if (minutes > -60) return rtf.format(minutes, "minutes")
  const hours = Math.round(minutes / 60)
  if (hours > -24) return rtf.format(hours, "hours")
  const days = Math.round(hours / 24)
  if (days > -30) return rtf.format(days, "days")
  const months = Math.round(days / 30)
  if (months > -12) return rtf.format(months, "months")
  return "a long time ago"
}

// node id -> the node of data.json, filled in by the page when it loads a
// network. A node that was removed from the configuration since an event was
// logged is not in here; those show up as "node <id>".
var state_nodes = new Map()

function node_name(node_id) {
  if (node_id == null) return "network"
  let node = state_nodes.get(node_id)
  return node != undefined ? node.name : `node ${node_id}`
}

// Keep the URL on the selected network, so it can be bookmarked and shared.
function set_url_network(network) {
  let url = new URL(window.location)
  url.searchParams.set(SEARCH_PARAM_NETWORK, network.slug)
  window.history.replaceState({}, "", url)
}

// The same two the tree page has in main.js, which these pages don't load.
// blocktree.js calls them from the block info boxes it draws.
function copyToClipboard(text, label) {
  navigator.clipboard.writeText(text)
    .then(() => showToast((label ? label + " " : "") + "copied to clipboard"))
    .catch(() => showToast("could not copy to clipboard"))
}

let toastTimer = null
function showToast(message) {
  let toast = document.getElementById("toast")
  if (toast == null) return
  toast.textContent = message
  toast.classList.add("toast-show")
  clearTimeout(toastTimer)
  toastTimer = setTimeout(() => toast.classList.remove("toast-show"), 1800)
}

// One entry per ActivityEventKind variant in src/activity.rs. `color` is the
// CSS variable the badge and the row marker are tinted with.
const ACTIVITY_KINDS = {
  "active-tip-changed": {
    label: "active tip changed",
    color: "var(--tip-status-color-active)",
    describe: d => `active tip ${block(d.old_hash, d.old_height)} → ${block(d.new_hash, d.new_height)}`,
  },
  "reorg-detected": {
    label: "reorg",
    color: "var(--tip-status-color-headers-only)",
    describe: d => `reorg ${d.depth} block${d.depth == 1 ? "" : "s"} deep: `
      + `${block(d.old_hash, d.old_height)} → ${block(d.new_hash, d.new_height)}`
      // for a one block reorg the fork point is the new tip; saying so twice
      // reads like two different blocks
      + (d.common_ancestor == d.new_hash ? ""
        : ` (forked off at ${block(d.common_ancestor, d.common_height)})`),
  },
  "tip-added": {
    label: "tip added",
    color: "var(--tip-status-color-valid-fork)",
    describe: d => `new ${status_chip(d.status)} tip ${block(d.hash, d.height)}`,
  },
  "tip-status-changed": {
    label: "tip status changed",
    color: "var(--tip-status-color-valid-headers)",
    describe: d => `tip ${block(d.hash, d.height)} `
      + `${status_chip(d.old_status)} → ${status_chip(d.new_status)}`,
  },
  "invalid-block-observed": {
    label: "invalid block",
    color: "var(--tip-status-color-invalid)",
    describe: d => `invalid block ${block(d.hash, d.height)}`,
  },
  "node-unreachable": {
    label: "unreachable",
    color: "var(--tip-status-color-invalid)",
    describe: () => "the node stopped responding",
  },
  "node-reachable": {
    label: "reachable",
    color: "var(--tip-status-color-active)",
    describe: () => "the node responded again",
  },
  "node-lagging": {
    label: "lagging",
    color: "var(--tip-status-color-headers-only)",
    describe: d => `at height ${num(d.node_height)}, `
      + `${num(d.best_height - d.node_height)} behind the best tip (${num(d.best_height)})`,
  },
  "node-caught-up": {
    label: "caught up",
    color: "var(--tip-status-color-active)",
    describe: d => `back at height ${num(d.node_height)}, best tip is ${num(d.best_height)}`,
  },
}

function kind_info(kind) {
  return ACTIVITY_KINDS[kind] || {
    label: kind,
    color: "var(--muted-soft)",
    // an event kind added to the backend but not here: show the raw details
    // rather than nothing at all.
    describe: d => mono(JSON.stringify(d)),
  }
}

function escape_html(text) {
  return String(text)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;")
}

function num(n) {
  return `<span class="ev-num">${escape_html(n)}</span>`
}

function mono(text) {
  return `<span class="ev-mono">${escape_html(text)}</span>`
}

// A block as "height …lasthexdigits", the hash copyable with a click. Same
// shortening as the node table's tip hashes.
function block(hash, height) {
  const short = typeof hash == "string" ? hash.substring(44, 64) : String(hash)
  return `<span class="ev-block">`
    + `<span class="ev-num">${escape_html(height)}</span>`
    + `<code class="ev-hash copyable" title="${escape_html(hash)} (click to copy)"`
    + ` onclick="copyToClipboard('${escape_html(hash)}', 'block hash')">…${escape_html(short)}</code>`
    + `</span>`
}

function status_chip(status) {
  return `<span class="status-chip tip-status-color-background-${escape_html(status)}">${escape_html(status)}</span>`
}

// The block hashes an event mentions, used by the free-text filter and by the
// playback page to highlight the blocks an event is about.
function event_hashes(event) {
  const d = event.details || {}
  return [d.hash, d.old_hash, d.new_hash, d.common_ancestor].filter(h => typeof h == "string")
}

// ---------------------------------------------------------------------------
// Replaying events onto node state
//
// The activity log records changes, not snapshots, so a node's tips at some
// point in the past are reconstructed by undoing events from the current state
// backwards. `undo_event` is the inverse of what src/activity.rs's diff_tips()
// logged, which makes the reconstruction exact for the states the events
// describe - with one gap: diff_tips only reports tips present in the *new*
// getchaintips result, so a tip that silently disappeared (a fork tip the node
// stopped listing) leaves no event and can't be restored. The other
// approximation is a reorg onto a branch the node already knew as a fork tip:
// undoing drops that tip instead of restoring its old status.
// ---------------------------------------------------------------------------

// state: { tips: Map(hash -> {hash, height, status}), reachable }
function undo_event(state, event) {
  const d = event.details || {}
  switch (event.kind) {
    case "active-tip-changed":
    case "reorg-detected":
      // the tip advanced onto (or reorged to) new_hash; before that the node
      // was active on old_hash, which the advance turned into an in-chain
      // block and dropped from the tip list.
      state.tips.delete(d.new_hash)
      state.tips.set(d.old_hash, { hash: d.old_hash, height: d.old_height, status: "active" })
      break
    case "tip-added":
      state.tips.delete(d.hash)
      break
    case "tip-status-changed":
      state.tips.set(d.hash, { hash: d.hash, height: d.height, status: d.old_status })
      break
    case "node-unreachable":
      state.reachable = true
      break
    case "node-reachable":
      state.reachable = false
      break
    // invalid-block-observed is always accompanied by a tip-added or
    // tip-status-changed event, which carries the status change; lagging and
    // caught-up are derived from the tip heights and change no state here.
  }
}

// Whether an event changes what the header tree shows for its node.
function affects_tips(event) {
  return event.kind == "active-tip-changed" || event.kind == "reorg-detected"
    || event.kind == "tip-added" || event.kind == "tip-status-changed"
}
