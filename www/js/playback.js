// Playback of the activity log on the header tree: reconstructs what the tips
// of every node looked like at the time of each logged event and hands that to
// the regular tree drawing code (blocktree.js), so the same visualization can
// be stepped through event by event.
//
// The reconstruction works backwards from the current state (data.json) by
// undoing events one at a time - see undo_event() in activity-events.js for
// what that can and cannot recover.

// --- globals blocktree.js expects from main.js on the tree page -------------
var state_data = { header_infos: [], nodes: [] }
var state_selected_network_id = 0
// the stratum jobs feed is a live-only feature and has no place in a replay of
// the past, so the tree draws without it.
const MINING_ENABLED = false
var state_stratum_jobs = new Map()
const nodeInfoRow = d3.select("#node_infos")
// ---------------------------------------------------------------------------

const networkSelect = d3.select("#network")
const playbackNow = document.getElementById("playback-now")
const playbackEvents = document.getElementById("playback-events")
const playbackSubtitle = document.getElementById("playback-subtitle")
const positionSlider = document.getElementById("position")
const playbackTimestamp = document.getElementById("playback-timestamp")
const playButton = document.getElementById("play")
const speedSelect = document.getElementById("speed")
const loadOlderButton = document.getElementById("load-older")
const footerCustom = document.getElementById("footer-custom")

var state_networks = []
// the network's current state, the anchor the reconstruction starts from
var state_current_data = { header_infos: [], nodes: [] }
// data.json's headers plus the blocks put back by reconstruct_headers()
var state_headers = []
// loaded events, oldest first (the API serves them newest first)
var state_events = []
// state_frames[k] is the state before event k; state_frames[events.length] is
// the current state. So the frame shown at position p has events 0..p-1 applied.
var state_frames = []
var state_position = 0
var state_exhausted = false
// ids of the nodes that have events in the loaded window; only those can be
// replayed. Nodes without `activity_log = true` never show up here.
var state_logged_nodes = new Set()
var play_timer = null

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

async function load_networks() {
  let networks = await fetch_json("api/networks.json")
  state_networks = networks.networks
  state_selected_network_id = network_from_url(state_networks).id

  networkSelect.selectAll("option")
    .data(state_networks)
    .enter()
    .append("option")
    .attr("value", d => d.id)
    .text(d => d.name)
    .property("selected", d => d.id == state_selected_network_id)
}

// Everything that changes when another network is selected.
async function load_network() {
  pause()
  let network = state_networks.find(n => n.id == state_selected_network_id)
  document.title = `${PAGE_NAME} - playback - ${network.name}`
  set_url_network(network)

  state_current_data = await fetch_json(`api/${network.id}/data.json`)
  state_nodes = new Map(state_current_data.nodes.map(node => [node.id, node]))
  state_events = await fetch_events()
  // a short page means the database has nothing older left, see load_older()
  state_exhausted = state_events.length < PAGE_SIZE
  build_frames()
  // start at the oldest loaded event, so pressing play replays everything
  set_position(0, { snap: true })
}

// A page of events, oldest first: the API serves them newest first, playback
// runs the other way round.
async function fetch_events(before) {
  let query = before == undefined ? "" : `?before=${before}`
  let data = await fetch_json(`api/${state_selected_network_id}/activity.json${query}`)
  return data.events.reverse()
}

// Extends the replay further into the past. The current state stays the anchor,
// so all frames are rebuilt from it.
async function load_older() {
  if (state_exhausted || state_events.length == 0) return
  loadOlderButton.disabled = true
  try {
    let added = await fetch_events(state_events[0].id)
    // a short page means the database has nothing older left
    state_exhausted = added.length < PAGE_SIZE
    if (added.length > 0) {
      state_events = added.concat(state_events)
      build_frames()
      // keep showing the same event, which moved back by the number added
      set_position(state_position + added.length, { snap: true })
    }
    update_controls()
  } catch (e) {
    console.error("could not load older activity events", e)
  } finally {
    loadOlderButton.disabled = false
  }
}

// ---------------------------------------------------------------------------
// Reconstructing blocks that data.json doesn't carry
//
// The backend only serves the headers it still finds interesting (see
// strip_tree() in src/headertree.rs) and reconnects what is left, which is what
// the tree draws as "N blocks hidden" links. Every one of those runs is a
// straight line of blocks, one per height - so a block an event names, which
// comes with its hash and its height, can be put back at exactly the position
// its height gives it. Without this, replaying anything but the last few
// minutes has almost nothing left to point at: the blocks that were the tip
// half an hour ago have long since stopped being interesting.
// ---------------------------------------------------------------------------

// A block only the activity log knows about. Its hash and height are the real
// ones; the rest of the header was never served to us.
function reconstructed_header(hash, height, id, prev_id) {
  return {
    id, prev_id, height, hash,
    version: 0,
    prev_blockhash: "",
    merkle_root: "",
    time: 0,
    bits: 0,
    // not MIN_DIFFICULTY, which would give the block the min-difficulty outline
    difficulty_int: 0,
    nonce: 0,
    miner: "",
    reconstructed: true,
  }
}

function reconstruct_headers(headers, events) {
  if (headers.length == 0) return headers

  let by_hash = new Map(headers.map(header => [header.hash, header]))
  let by_id = new Map(headers.map(header => [header.id, header]))

  // Where the events say a block sits. Only active-tip-changed and
  // reorg-detected relate two blocks to each other; the other kinds name a
  // block without saying anything about its ancestry, so a fork tip that has
  // dropped out of data.json can't be placed and stays out.
  //
  // Following each node's active tip from event to event gives the chain it
  // walked. A reorg cuts that chain at the fork point: everything the node had
  // above it turns out to have been a branch, however long it looked like the
  // chain at the time, so those blocks move off to the side instead of being
  // put back into the run of hidden blocks.
  let chains = new Map()  // node id -> the blocks its active tip visited, in order
  let branches = []       // {ancestor, blocks}, the branches reorgs left behind
  events.forEach(event => {
    let d = event.details || {}
    let chain = chains.get(event.node_id)
    if (chain == undefined) {
      chain = []
      chains.set(event.node_id, chain)
    }
    if (event.kind == "active-tip-changed") {
      // the first event of a node also tells us where it was coming from
      if (chain.length == 0) chain.push({ hash: d.old_hash, height: d.old_height })
      chain.push({ hash: d.new_hash, height: d.new_height })
    } else if (event.kind == "reorg-detected") {
      let left = chain.filter(block => block.height > d.common_height)
      if (!left.some(block => block.hash == d.old_hash)) {
        left.push({ hash: d.old_hash, height: d.old_height })
      }
      branches.push({ ancestor: d.common_ancestor, blocks: left })
      chains.set(event.node_id, chain
        .filter(block => block.height <= d.common_height)
        .concat([
          { hash: d.common_ancestor, height: d.common_height },
          { hash: d.new_hash, height: d.new_height },
        ]))
    }
  })

  let on_chain = new Map() // hash -> height
  chains.forEach(chain => chain.forEach(block => on_chain.set(block.hash, block.height)))
  // A block on a branch some node was reorged off is not on the chain, no
  // matter what another node's events said about it before.
  branches.forEach(branch => branch.blocks.forEach(block => on_chain.delete(block.hash)))

  // The chain the hidden runs belong to: from the highest block down to the
  // root, lowest first.
  let path = []
  for (let header = headers.reduce((a, b) => (b.height > a.height ? b : a));
       header != undefined;
       header = by_id.get(header.prev_id)) {
    path.push(header)
  }
  path.reverse()

  // Group the missing blocks by the gap their height falls into. A block whose
  // height is spanned by a link without a gap, or that sits above the tip or
  // below the root of that chain, has no run of hidden blocks to belong to.
  let gaps = new Map() // the header to splice in front of -> the blocks to add
  on_chain.forEach((height, hash) => {
    if (by_hash.has(hash)) return
    let index = path.findIndex(header => header.height > height)
    if (index <= 0) return
    let parent = path[index - 1]
    let child = path[index]
    // no run of hidden blocks here, or this chain already has a block at that
    // height and the missing one is a fork off it we can't place
    if (child.height - parent.height <= 1 || parent.height >= height) return
    if (!gaps.has(child)) gaps.set(child, { parent, blocks: [] })
    gaps.get(child).blocks.push({ hash, height })
  })

  let next_id = headers.reduce((max, header) => Math.max(max, header.id), 0) + 1
  let added = []
  // the headers whose prev_id has to move to a reconstructed block
  let rewired = new Map()

  // Puts blocks in height order behind `prev_id`, reconstructing the ones that
  // aren't in the tree yet, and returns the id of the last one.
  function chain_behind(prev_id, blocks) {
    blocks.sort((a, b) => a.height - b.height).forEach(block => {
      let header = by_hash.get(block.hash)
      if (header == undefined) {
        header = reconstructed_header(block.hash, block.height, next_id++, prev_id)
        added.push(header)
        by_hash.set(block.hash, header)
      }
      prev_id = header.id
    })
    return prev_id
  }

  gaps.forEach((gap, child) => rewired.set(child, chain_behind(gap.parent.id, gap.blocks)))

  // A branch a reorg left behind hangs off the common ancestor, which by now is
  // in place if the events named it. Where the node never reported one of the
  // branch's blocks, the link to the next is drawn as the run of hidden blocks
  // it is.
  branches.forEach(branch => {
    let ancestor = by_hash.get(branch.ancestor)
    if (ancestor != undefined) chain_behind(ancestor.id, branch.blocks)
  })

  // Rewiring replaces the header rather than mutating it: the frames share
  // these objects, and a frame that stops short of a reconstructed block would
  // otherwise be left pointing at a parent it doesn't contain.
  return headers
    .map(header => {
      let prev_id = rewired.get(header)
      return prev_id === undefined ? header : Object.assign({}, header, { prev_id })
    })
    .concat(added)
}

// A block only stays in the tree while the tree finds it interesting, which for
// a block on the active chain means while it is the tip. Replaying tip movement
// would therefore drop each block back into the run of hidden blocks as soon as
// the next one arrived: one block coming in, the one before it going out, and
// the layout shifting under both. Marking every block the loaded events name -
// the whole set up front, so the tree looks the same at a position however it
// was reached - keeps them all in place for the length of the replay.
function keep_replayed_blocks_visible(headers, events) {
  let named = new Set()
  events.forEach(event => event_hashes(event).forEach(hash => named.add(hash)))
  headers.forEach(header => {
    if (named.has(header.hash)) header.keep_visible = true
  })
}

// ---------------------------------------------------------------------------
// Reconstructing the states
// ---------------------------------------------------------------------------

function build_frames() {
  state_headers = reconstruct_headers(state_current_data.header_infos, state_events)
  keep_replayed_blocks_visible(state_headers, state_events)

  // the state we know for certain: what the nodes report right now
  let current = {}
  state_current_data.nodes.forEach(node => {
    current[node.id] = {
      tips: new Map(node.tips.map(tip => [tip.hash, tip])),
      reachable: node.reachable,
    }
  })

  let frames = new Array(state_events.length + 1)
  frames[state_events.length] = { nodes: current, timestamp: null }
  for (let k = state_events.length - 1; k >= 0; k--) {
    let event = state_events[k]
    let after = frames[k + 1]
    let node = after.nodes[event.node_id]
    if (node == undefined) {
      // an event of a node that is not in data.json anymore: nothing to undo
      frames[k] = { nodes: after.nodes, timestamp: event.timestamp }
      continue
    }
    let undone = {
      tips: new Map(node.tips),
      reachable: node.reachable,
    }
    undo_event(undone, event)
    frames[k] = {
      nodes: Object.assign({}, after.nodes, { [event.node_id]: undone }),
      timestamp: event.timestamp,
    }
  }

  // When a node's tips last changed, forward from the oldest loaded event, so
  // the node table's "tip changed" column moves along with the replay.
  let changed_at = {}
  frames[0].changed_at = changed_at
  state_events.forEach((event, k) => {
    if (affects_tips(event)) {
      changed_at = Object.assign({}, changed_at, { [event.node_id]: event.timestamp })
    }
    frames[k + 1].changed_at = changed_at
    // a frame is reached by applying its event, so that is the time it shows
    frames[k + 1].timestamp = event.timestamp
  })

  state_frames = frames
  state_logged_nodes = new Set(state_events.map(event => event.node_id))
  positionSlider.max = state_events.length
}

// The state of a frame in the shape data.json has, so blocktree.js can draw it.
function frame_data(frame) {
  // Only the nodes that have events can be replayed. One without them is stuck
  // at its current state for the whole replay, which reads as if it had been
  // there all along - and it drags the tree along with it, because the tip
  // status boxes aggregate over all nodes.
  let nodes = state_current_data.nodes
    .filter(node => state_logged_nodes.has(node.id))
    .map(node => {
      let state = frame.nodes[node.id]
      // the node's version is not replayed (it isn't logged), so the one it
      // reports now is carried over from `node`
      return Object.assign({}, node, {
        tips: [...state.tips.values()],
        reachable: state.reachable,
        // When the node's tips last changed: known for a change inside the
        // replayed window, otherwise the node's current value - which only
        // says anything if it is older than the frame, as everything after it
        // is a change the replay hasn't reached yet.
        last_changed_timestamp: frame.changed_at[node.id]
          || Math.min(node.last_changed_timestamp, frame.timestamp),
      })
    })

  // Only draw the part of the chain that existed back then. There is no record
  // of when a header was learned, so the best available approximation is the
  // highest tip any node had at this point: everything above it is future.
  // Filtering by height keeps the tree connected, as a block's parent is always
  // one lower.
  let max_tip = 0
  nodes.forEach(node => node.tips.forEach(tip => {
    if (tip.height > max_tip) max_tip = tip.height
  }))
  // A reconstructed block always sits above its parent and below the header it
  // was spliced in front of, so cutting by height can't orphan anything.
  let header_infos = max_tip == 0
    ? state_headers
    : state_headers.filter(header => header.height <= max_tip)

  return { header_infos, nodes }
}

// ---------------------------------------------------------------------------
// Showing a position
// ---------------------------------------------------------------------------

function set_position(position, opts) {
  opts = opts || {}
  state_position = Math.max(0, Math.min(position, state_events.length))
  positionSlider.value = state_position
  let frame = state_frames[state_position]
  if (frame == undefined) return

  state_data = frame_data(frame)
  let blocks = []
  if (state_data.header_infos.length > 0) {
    draw({ reason: "playback", snap: !!opts.snap })
    blocks = mark_blocks()
    focus_blocks(blocks, !!opts.snap)
  }
  draw_nodes()
  render_timestamp()
  render_now(blocks.length)
  render_events()
  update_controls()
}

// The event that got us to the current position, or null at the very start.
function current_event() {
  return state_position == 0 ? null : state_events[state_position - 1]
}

// Marks the blocks that only the activity log knows about, and picks out where
// the current event's blocks ended up. What shows where something happened is
// the tree itself - the tip status boxes moving between blocks, and a new block
// coming in with the animation a new block always gets - so the event's blocks
// are not singled out on top of that; their positions are only used to pan
// there, and their number to tell when an event is about something the tree
// can't show at all.
function mark_blocks() {
  let event = current_event()
  let hashes = event == null ? [] : event_hashes(event)
  let positions = []
  let reconstructed = d => d.data.data.reconstructed === true
  g.selectAll(".block")
    .classed("playback-reconstructed", reconstructed)
    .each(function (d) {
      // the block group keeps its layout position in x/y, kept in sync by draw()
      if (hashes.includes(d.data.data.hash)) {
        positions.push([+this.getAttribute("x"), +this.getAttribute("y")])
      }
    })
  g.selectAll(".block-back").classed("playback-reconstructed", reconstructed)
  return positions
}

// Pans the tree to the blocks the current event is about. draw() moves the
// camera only when the highest tip changes, so an event on a fork branch, or a
// reorg back onto a lower tip, would otherwise happen off screen. The blocks
// are anchored where the tip normally sits, so a replay of plain tip movement
// still frames the chain the way the live page does. An event with no block of
// its own drawn - a node going unreachable, or a block data.json has dropped -
// leaves the camera where draw() put it.
function focus_blocks(positions, snap) {
  if (positions.length == 0) return
  let xs = positions.map(p => p[0])
  let ys = positions.map(p => p[1])
  let size = d3.select("#drawing-area").node().getBoundingClientRect()
  zoom.translateTo(
    svg.transition(d3.transition().duration(snap ? 0 : 400)),
    (Math.min(...xs) + Math.max(...xs)) / 2,
    (Math.min(...ys) + Math.max(...ys)) / 2,
    o.tip_anchor(size.width, size.height))
}

function format_time(timestamp) {
  return new Date(timestamp * 1000).toLocaleString()
}

// The moment the replay is showing, over the tree, which is where you are
// looking while it plays: the time of the event that got us here, or of the
// oldest loaded event at the very start, since the state shown then is the one
// just before it.
function render_timestamp() {
  if (state_events.length == 0) {
    playbackTimestamp.innerHTML = ""
    return
  }
  let timestamp = (current_event() || state_events[0]).timestamp
  let time = new Date(timestamp * 1000)
  playbackTimestamp.innerHTML = `
    <span class="pb-timestamp-time">${time.toLocaleTimeString()}</span>
    <span class="pb-timestamp-date">${time.toLocaleDateString()} · ${relative_time(timestamp)}</span>`
}

function render_now(drawn) {
  let event = current_event()
  if (event == null) {
    playbackNow.innerHTML = state_events.length == 0
      ? `<span class="text-muted-soft">No activity events for this network. Either nothing
         has happened yet, or the activity log is not enabled: it needs an
         <code>[activity]</code> section in the configuration and
         <code>activity_log = true</code> on the nodes that should be logged.</span>`
      : `<span class="text-muted-soft">Before the oldest loaded event
         (${escape_html(format_time(state_events[0].timestamp))}). Press play.</span>`
    return
  }
  let info = kind_info(event.kind)
  // data.json only carries the blocks that are still interesting; the ones an
  // older event is about may have been dropped from it since, and then there
  // is nothing to point at in the tree.
  let missing = drawn == 0 && event_hashes(event).length > 0
  playbackNow.innerHTML = `
    <div class="pb-now-row" style="--event-color: ${info.color}">
      <span class="ev-badge">${escape_html(info.label)}</span>
      <span class="pb-now-node">${escape_html(node_name(event.node_id))}</span>
      <span class="pb-now-detail">${info.describe(event.details || {})}${missing
        ? ` <span class="text-muted-soft small"
            title="data.json only serves the blocks that are still interesting, so this one can't be drawn"
            >(not in the drawn tree)</span>`
        : ""}</span>
      <span class="pb-now-time text-muted-soft">${escape_html(format_time(event.timestamp))}</span>
      <span class="text-muted-soft small">${state_position} / ${state_events.length}</span>
    </div>`
}

// The event list is rebuilt only when the events themselves change; stepping
// through them just re-marks the rows, which keeps playing at the fast speeds
// from rebuilding a few hundred DOM nodes per step.
var rendered_events = null

function render_events() {
  if (rendered_events !== state_events) {
    // newest at the top, like the activity list
    playbackEvents.innerHTML = state_events.map((event, index) => {
      let info = kind_info(event.kind)
      return `<button type="button" class="pb-event" data-position="${index + 1}"
          style="--event-color: ${info.color}" title="${format_time(event.timestamp)}">
          <span class="pb-event-kind">${escape_html(info.label)}</span>
          <span class="pb-event-node">${escape_html(node_name(event.node_id))}</span>
          <span class="pb-event-detail">${info.describe(event.details || {})}</span>
        </button>`
    }).reverse().join("")
    rendered_events = state_events
  }

  playbackEvents.querySelectorAll(".pb-event").forEach(row => {
    let position = parseInt(row.dataset.position)
    row.classList.toggle("is-current", position == state_position)
    row.classList.toggle("is-future", position > state_position)
  })

  // keep the current event in the middle of the panel. scrollIntoView() would
  // do this too, but it scrolls every ancestor - including the page, which
  // would jump around on every step.
  let current = playbackEvents.querySelector(".is-current")
  if (current != null) {
    playbackEvents.scrollTop = Math.max(0,
      current.offsetTop - (playbackEvents.clientHeight - current.offsetHeight) / 2)
  }
}

function update_controls() {
  playButton.textContent = play_timer == null ? "▶ play" : "▮▮ pause"
  playButton.classList.toggle("is-playing", play_timer != null)
  loadOlderButton.style.display = state_exhausted ? "none" : ""

  let network = state_networks.find(n => n.id == state_selected_network_id)
  if (network != undefined) {
    playbackSubtitle.textContent = state_events.length == 0
      ? `No events to replay for ${network.name}.`
      : `Replaying ${state_events.length} activity events of ${network.name} on the header tree.`
  }
}

// ---------------------------------------------------------------------------
// Play / pause
// ---------------------------------------------------------------------------

function play() {
  if (play_timer != null || state_events.length == 0) return
  // starting from the end replays from the beginning again
  if (state_position >= state_events.length) set_position(0, { snap: true })
  play_timer = setInterval(() => {
    if (state_position >= state_events.length) {
      pause()
      return
    }
    set_position(state_position + 1)
  }, parseInt(speedSelect.value))
  update_controls()
}

function pause() {
  if (play_timer != null) {
    clearInterval(play_timer)
    play_timer = null
  }
  update_controls()
}

function toggle_play() {
  if (play_timer == null) play()
  else pause()
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

// Everything but playing itself moves the replay by hand, which always stops it
// first. A jump snaps the tree into place instead of animating there, which
// only makes sense between neighbouring positions.
function go(position, snap) {
  pause()
  set_position(position, { snap: !!snap })
}

const buttons = {
  "play": toggle_play,
  "step-forward": () => go(state_position + 1),
  "step-back": () => go(state_position - 1),
  "to-start": () => go(0, true),
  "to-end": () => go(state_events.length, true),
  "load-older": load_older,
}
Object.entries(buttons).forEach(([id, handler]) =>
  document.getElementById(id).addEventListener("click", handler))

const keys = {
  " ": toggle_play,
  "ArrowRight": () => go(state_position + 1),
  "ArrowLeft": () => go(state_position - 1),
  "Home": () => go(0, true),
  "End": () => go(state_events.length, true),
  "Escape": closeAllDescriptions,
}
document.addEventListener("keydown", (e) => {
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement) return
  let handler = keys[e.key]
  if (handler == undefined) return
  e.preventDefault()
  handler()
})

positionSlider.addEventListener("input", () => go(parseInt(positionSlider.value), true))

playbackEvents.addEventListener("click", (e) => {
  let button = e.target.closest(".pb-event")
  if (button != null) go(parseInt(button.dataset.position), true)
})

speedSelect.addEventListener("input", () => {
  // restart the timer so a speed change takes effect right away
  if (play_timer != null) {
    pause()
    play()
  }
})

networkSelect.on("input", async function () {
  state_selected_network_id = parseInt(this.value)
  // the tree on screen is the old network's; show the loading overlay so it isn't
  // mistaken for the new one while it loads. draw() hides it again once the new
  // network's tree is drawn (see load_network() -> set_position(0, { snap: true })).
  show_viz_loading()
  await load_network()
})

async function run() {
  try {
    await load_networks()
    await load_network()
  } catch (e) {
    console.error("could not load the playback data", e)
    playbackNow.innerHTML = `<span class="text-muted-soft">could not load the activity log</span>`
  }

  fetch_json("api/info.json")
    .then(info => footerCustom.innerHTML = info.footer)
    .catch(console.error)

  // the clock's "x ago" ages while the page is open
  setInterval(render_timestamp, 10000)
}

run()
