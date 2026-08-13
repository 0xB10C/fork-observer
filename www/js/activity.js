// The activity log as an HTML list: the events of api/<network>/activity.json,
// newest first, filterable by event kind, node and free text, and extendable
// into the past with the `before` cursor of the API.

// A cache_changed event means a node reported something; the activity writer
// persists the corresponding events a moment later, so give it a head start
// and coalesce the burst of events a new block causes.
const REFRESH_DELAY_MS = 1000
// Backstop for the SSE-driven refresh, in case the event stream is down.
const REFRESH_INTERVAL_MS = 20000

const networkSelect = document.getElementById("network")
const connectionStatus = document.getElementById("connection-status")
const activityList = document.getElementById("activity-list")
const activityStatus = document.getElementById("activity-status")
const activitySubtitle = document.getElementById("activity-subtitle")
const kindFilters = document.getElementById("kind-filters")
const nodeFilters = document.getElementById("node-filters")
const searchInput = document.getElementById("search")
const liveCheckbox = document.getElementById("live")
const loadOlderButton = document.getElementById("load-older")
const loadOlderNote = document.getElementById("load-older-note")
const footerCustom = document.getElementById("footer-custom")

var state_networks = []
var state_network = null
// all loaded events of the selected network, newest first
var state_events = []
// event kinds and node ids that are hidden; empty means "show everything"
var state_hidden_kinds = new Set()
var state_hidden_nodes = new Set()
var state_search = ""
// no older events left in the activity database
var state_exhausted = false
// ids of the events that arrived while the page was open, so they can be
// highlighted as they come in
var state_fresh = new Set()
var refresh_timer = null

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

async function load_networks() {
  let networks = await fetch_json("api/networks.json")
  state_networks = networks.networks
  state_network = network_from_url(state_networks)

  networkSelect.replaceChildren(...state_networks.map(network => {
    let option = document.createElement("option")
    option.value = network.id
    option.textContent = network.name
    option.selected = network.id == state_network.id
    return option
  }))
}

// Everything that changes when another network is selected.
async function load_network() {
  document.title = `${PAGE_NAME} - activity - ${state_network.name}`
  activitySubtitle.textContent = `What the nodes of ${state_network.name} reported, newest first.`
  set_url_network(state_network)
  activityList.innerHTML = `<p class="activity-empty text-muted-soft">loading…</p>`

  try {
    // node names for the node column and the node filter
    let data = await fetch_json(`api/${state_network.id}/data.json`)
    state_nodes = new Map(data.nodes.map(node => [node.id, node]))

    state_events = await fetch_events()
    // a short page means the database has nothing older left, see load_older()
    state_exhausted = state_events.length < PAGE_SIZE
    state_fresh = new Set()
    render()
  } catch (e) {
    console.error("could not load the activity log", e)
    activityList.innerHTML =
      `<p class="activity-empty text-muted-soft">could not load the activity log</p>`
  }
}

// A page of events, newest first. Without `before` this is the newest page.
async function fetch_events(before) {
  let query = before == undefined ? "" : `?before=${before}`
  let data = await fetch_json(`api/${state_network.id}/activity.json${query}`)
  return data.events
}

// Extends the list into the past, from the oldest event we have.
async function load_older() {
  if (state_exhausted || state_events.length == 0) return
  loadOlderButton.disabled = true
  loadOlderNote.textContent = "loading…"
  try {
    let older = await fetch_events(state_events[state_events.length - 1].id)
    state_events = state_events.concat(older)
    // A short page means the database has nothing older left. Events of all
    // networks share one id sequence, so the row ids are not contiguous per
    // network and only the page length tells us this.
    state_exhausted = older.length < PAGE_SIZE
    render()
  } catch (e) {
    console.error("could not load older activity events", e)
    loadOlderNote.textContent = "could not load older events"
  } finally {
    loadOlderButton.disabled = false
  }
}

// Picks up events logged since the last refresh. The API serves recent events
// from a ring buffer, so this is cheap enough to call on every change.
async function refresh_events() {
  let events = await fetch_events()
  let newest = state_events.length == 0 ? -1 : state_events[0].id
  let fresh = events.filter(event => event.id > newest)
  if (fresh.length == 0) return

  fresh.forEach(event => state_fresh.add(event.id))
  if (fresh.length == events.length) {
    // No overlap: more happened than fits in one page, so what we have is not
    // contiguous with what we just fetched. Start over from this page.
    state_events = events
    state_exhausted = false
  } else {
    state_events = fresh.concat(state_events)
  }
  render()
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

function build_filters() {
  // Every kind the backend can log, so a kind can be filtered for before an
  // event of it has ever been seen.
  kindFilters.replaceChildren(...Object.keys(ACTIVITY_KINDS).map(kind => {
    let info = kind_info(kind)
    return filter_pill(info.label, info.color, state_hidden_kinds, kind)
  }))

  // Only the nodes that turn up in the loaded events: the network usually has
  // more - nodes without `activity_log = true`, and the nodes of remote
  // fork-observers - and none of them can ever have an event to filter.
  let node_ids = new Set(state_events.map(event => event.node_id).filter(id => id != null))
  nodeFilters.replaceChildren(...[...node_ids].sort((a, b) => a - b).map(id =>
    filter_pill(node_name(id), "var(--muted-soft)", state_hidden_nodes, id)))
}

// A toggle: on (the default) means events of this kind/node are shown. It
// stores the *hidden* ones, so kinds and nodes that turn up later are shown.
function filter_pill(label, color, hidden, key) {
  let pill = document.createElement("button")
  pill.type = "button"
  pill.className = "filter-pill"
  pill.style.setProperty("--pill-color", color)
  pill.textContent = label
  pill.classList.toggle("is-off", hidden.has(key))
  pill.addEventListener("click", () => {
    if (hidden.has(key)) hidden.delete(key)
    else hidden.add(key)
    pill.classList.toggle("is-off", hidden.has(key))
    render_list()
  })
  return pill
}

function matches_filters(event) {
  if (state_hidden_kinds.has(event.kind)) return false
  if (event.node_id != null && state_hidden_nodes.has(event.node_id)) return false
  if (state_search == "") return true
  return [
    event.kind,
    kind_info(event.kind).label,
    node_name(event.node_id),
    JSON.stringify(event.details),
  ].join(" ").toLowerCase().includes(state_search)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const EMPTY_HTML = `<p class="activity-empty text-muted-soft">
  No activity events for this network.<br>
  Either nothing has happened yet, or the activity log is not enabled: it needs an
  <code>[activity]</code> section in the configuration and <code>activity_log = true</code>
  on the nodes that should be logged.
</p>`

const NO_MATCH_HTML =
  `<p class="activity-empty text-muted-soft">No loaded event matches the current filters.</p>`

function node_description(node_id) {
  let node = state_nodes.get(node_id)
  if (node == undefined) return "this node is not in the configuration anymore"
  return [node.description, node.version].filter(s => s).join(" — ")
}

function day_of(timestamp) {
  return new Date(timestamp * 1000).toLocaleDateString(undefined,
    { weekday: "short", year: "numeric", month: "short", day: "numeric" })
}

function render() {
  build_filters()
  render_list()
}

function render_list() {
  let events = state_events.filter(matches_filters)
  let loaded = state_events.length

  activityStatus.textContent = loaded == 0 ? "" :
    (events.length == loaded
      ? `${loaded} events loaded`
      : `${events.length} of ${loaded} loaded events shown`)
    + `, oldest ${relative_time(state_events[loaded - 1].timestamp)}`

  activityList.innerHTML =
    loaded == 0 ? EMPTY_HTML :
    events.length == 0 ? NO_MATCH_HTML :
    render_rows(events)

  loadOlderButton.style.display = loaded > 0 && !state_exhausted ? "" : "none"
  loadOlderNote.textContent =
    loaded > 0 && state_exhausted ? "no older events in the database" : ""
}

function render_rows(events) {
  let html = ""
  let last_day = null
  events.forEach(event => {
    let day = day_of(event.timestamp)
    if (day != last_day) {
      html += `<div class="activity-day">${day}</div>`
      last_day = day
    }
    html += render_row(event)
  })
  return html
}

function render_row(event) {
  let info = kind_info(event.kind)
  let time = new Date(event.timestamp * 1000)
  return `
    <div class="activity-row${state_fresh.has(event.id) ? " is-fresh" : ""}"
         style="--event-color: ${info.color}">
      <div class="ev-time" title="${time.toISOString()}">
        <span class="ev-clock">${time.toLocaleTimeString()}</span>
        <span class="ev-ago" data-timestamp="${event.timestamp}">${relative_time(event.timestamp)}</span>
      </div>
      <div class="ev-kind"><span class="ev-badge">${escape_html(info.label)}</span></div>
      <div class="ev-node" title="${escape_html(node_description(event.node_id))}"
        >${escape_html(node_name(event.node_id))}</div>
      <div class="ev-detail">${info.describe(event.details || {})}</div>
      <div class="ev-id" title="activity log row id, the cursor for ?before=">#${event.id}</div>
    </div>`
}

// ---------------------------------------------------------------------------
// Live updates
// ---------------------------------------------------------------------------

const changeSSE = new EventSource("api/changes")

changeSSE.addEventListener("open", () => {
  connectionStatus.style.color = "var(--tip-status-color-active)"
  connectionStatus.title = "connected — receiving live updates"
})

changeSSE.addEventListener("error", (e) => {
  console.error("SSE error", e)
  connectionStatus.style.color = "var(--tip-status-color-invalid)"
  connectionStatus.title = "disconnected — reconnecting…"
})

changeSSE.addEventListener("cache_changed", (e) => {
  // the stream can beat the first networks.json response
  if (state_network == null) return
  if (JSON.parse(e.data).network_id == state_network.id) schedule_refresh()
})

// The server dropped events for us because we fell behind, so we don't know
// whether any of them were ours. Refresh and find out.
changeSSE.addEventListener("events_missed", (e) => {
  console.debug("missed events, refreshing: ", e.data)
  if (state_network != null) schedule_refresh()
})

function schedule_refresh() {
  if (!liveCheckbox.checked || refresh_timer != null) return
  refresh_timer = setTimeout(async () => {
    refresh_timer = null
    try {
      await refresh_events()
    } catch (e) {
      console.error("could not refresh activity events", e)
    }
  }, REFRESH_DELAY_MS)
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

networkSelect.addEventListener("input", async () => {
  state_network = state_networks.find(n => n.id == parseInt(networkSelect.value))
  await load_network()
})

searchInput.addEventListener("input", () => {
  state_search = searchInput.value.trim().toLowerCase()
  render_list()
})

loadOlderButton.addEventListener("click", load_older)

async function run() {
  try {
    await load_networks()
  } catch (e) {
    console.error("could not load the networks", e)
    return
  }
  await load_network()

  fetch_json("api/info.json")
    .then(info => footerCustom.innerHTML = info.footer)
    .catch(console.error)

  // relative timestamps age while the page is open
  setInterval(() => {
    document.querySelectorAll(".ev-ago").forEach(el => {
      el.textContent = relative_time(parseInt(el.dataset.timestamp))
    })
  }, 10000)

  // backstop in case the SSE stream is down
  setInterval(schedule_refresh, REFRESH_INTERVAL_MS)
}

run()
