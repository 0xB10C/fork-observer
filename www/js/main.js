const getNetworks = new Request('api/networks.json');
const getInfo = new Request('api/info.json');
const changeSSE = new EventSource('api/changes');

const networkSelect = d3.select("#network")
const nodeInfoRow = d3.select("#node_infos")
const networkInfoDescription = d3.select("#network_info_description")
const networkInfoName = d3.select("#network_info_name")
const footerCustom = d3.select("#footer-custom")
const connectionStatus = d3.select("#connection-status")
const rssRecentForks = d3.select("#rss_recent_forks")
const rssInvalidBlocks = d3.select("#rss_invalid_blocks")
const rssLaggingNodes = d3.select("#rss_lagging_nodes")
const rssUnreachableNodes = d3.select("#rss_unreachable_nodes")

const soundCheckbox = d3.select("#sound")

const SEARCH_PARAM_NETWORK = "network"

// The mining feature (see the stratum jobs section at the bottom) is opt-in via
// ?mining. Declared up here because blocktree.js reads it while drawing, which can
// happen before the bottom of this file has run.
const MINING_ENABLED = new URLSearchParams(window.location.search).has("mining")

// TODO: should be queried via the API as info
const PAGE_NAME = "fork-observer"

var state_selected_network_id = 0
var state_networks = []
var state_data = {}
// update_cooling_down: an update ran within the last UPDATE_COOLDOWN_MS.
// update_scheduled: more events arrived while cooling down, so run one more after.
var update_cooling_down = false
var update_scheduled = false
var state_sound_enabled = false
// hash of the active chain tip as of the last update, or null if we haven't seen one
// yet (initial load, or right after switching networks)
var state_tip_hash = null

async function fetch_info() {
  console.debug("called fetch_info()")
  await fetch(getInfo)
    .then(response => response.json())
    .then(info => {
      footerCustom.html(info.footer)
    }).catch(console.error);
}

async function fetch_data() {
  console.debug("called fetch_data()")
  await fetch(`api/${state_selected_network_id}/data.json`)
    .then(response => response.json())
    .then(data => state_data = data)
    .catch(console.error);
}

async function fetch_networks() {
  console.debug("called fetch_networks()")
  await fetch(getNetworks)
    .then(response => response.json())
    .then(networks => {
      state_networks = networks.networks
      set_initial_network()
      update_network()
    }).catch(console.error);
}

function update_network() {
  console.debug("called update_network()")
  // the tip of the previously selected network says nothing about the new one, so
  // forget it to avoid a spurious sound on the first update after switching
  state_tip_hash = null
  let current_network = state_networks.filter(net => net.id == state_selected_network_id)[0]
  document.title = PAGE_NAME + " - " + current_network.name;
  networkInfoName.text(current_network.name)
  networkInfoDescription.text(current_network.description)
  rssRecentForks.node().href = `rss/${current_network.id}/forks.xml`
  rssInvalidBlocks.node().href = `rss/${current_network.id}/invalid.xml`
  rssLaggingNodes.node().href = `rss/${current_network.id}/lagging.xml`
  rssUnreachableNodes.node().href = `rss/${current_network.id}/unreachable.xml`

  // Keep the URL in sync with the selected network, using the friendly slug, so
  // it can be bookmarked and shared (e.g. ?network=testnet4).
  let url = new URL(window.location)
  url.searchParams.set(SEARCH_PARAM_NETWORK, current_network.slug)
  window.history.replaceState({}, "", url)
}

function set_initial_network() {
  console.debug("called set_initial_network()")
  let url = new URL(window.location);
  let searchParams = new URLSearchParams(url.search);
  let searchParamNetwork = searchParams.get(SEARCH_PARAM_NETWORK)

  // Match the URL parameter against the network slug or, for backwards
  // compatibility, the numeric network id.
  let matched = state_networks.find(x => x.slug == searchParamNetwork || x.id == searchParamNetwork)
  if (searchParamNetwork != null && matched != undefined) {
    console.debug("Setting network to", searchParamNetwork, "based on the URL search parameter", SEARCH_PARAM_NETWORK)
    state_selected_network_id = matched.id
  } else {
    console.debug("Setting network to first network:", state_networks[0].id);
    state_selected_network_id = state_networks[0].id
  }

  networkSelect.selectAll('option')
    .data(state_networks)
    .enter()
      .append('option')
      .attr('value', d => d.id)
      .text(d => d.name)
      .property("selected", d => d.id == state_selected_network_id)
}

// The notification sound, synthesized with the Web Audio API so no audio file has to
// be shipped and served. A dry "tock", like a knock on a wooden desk: a short tone
// that drops slightly in pitch as it dies away. The drop has to be small and quick —
// a wide pitch glide, upward especially, is what turns a knock into a wet bubble.
const SOUND_POP = {
  freq_start: 620,
  freq_end: 430,
  bend: 0.03,      // seconds the pitch takes to settle
  duration: 0.11,  // seconds until the tone has decayed away
}
// the tock is the fundamental plus a quiet, very short overtone. The overtone is only
// there for the click of the attack, which is the part that carries over background
// noise and keeps the sound from turning into a soft thud.
const SOUND_PARTIALS = [
  { ratio: 1, volume: 0.6, decay: 1 },
  { ratio: 3, volume: 0.12, decay: 0.2 },
]

let audioCtx = null

// The context is created on the first play, which only ever happens from the
// checkbox click or, once enabled, from a tip change. Browsers require a user
// gesture before audio can start, and the click provides it. A context can also be
// suspended again later (e.g. a backgrounded tab), hence the resume.
function ensure_audio_context() {
  if (audioCtx === null) {
    let AudioCtx = window.AudioContext || window.webkitAudioContext
    if (AudioCtx === undefined) {
      console.warn("no Web Audio API support: cannot play the chain tip sound")
      return null
    }
    audioCtx = new AudioCtx()
  }
  if (audioCtx.state == "suspended") {
    audioCtx.resume().catch(console.debug)
  }
  return audioCtx
}

function play_sound() {
  let ctx = ensure_audio_context()
  if (ctx === null) return

  let start = ctx.currentTime
  SOUND_PARTIALS.forEach(partial => {
    let osc = ctx.createOscillator()
    osc.type = "sine"
    osc.frequency.setValueAtTime(SOUND_POP.freq_start * partial.ratio, start)
    osc.frequency.exponentialRampToValueAtTime(SOUND_POP.freq_end * partial.ratio, start + SOUND_POP.bend)

    // near-instant attack followed by an exponential decay, so it bursts rather than
    // fades in. Exponential ramps can't touch zero, hence the small non-zero start
    // and end values.
    let stop = start + SOUND_POP.duration * partial.decay
    let gain = ctx.createGain()
    gain.gain.setValueAtTime(0.0001, start)
    gain.gain.exponentialRampToValueAtTime(partial.volume, start + 0.004)
    gain.gain.exponentialRampToValueAtTime(0.0001, stop)

    osc.connect(gain)
    gain.connect(ctx.destination)
    osc.start(start)
    osc.stop(stop + 0.02)
  })
}

// The hash of the current chain tip: the highest tip our nodes consider active. Ties
// (multiple nodes active on different blocks of the same height) are broken by hash,
// so the same situation always produces the same result.
function current_tip_hash() {
  let best = null
  if (state_data.nodes == undefined) return null
  state_data.nodes.forEach(node => {
    node.tips.filter(tip => tip.status == "active").forEach(tip => {
      if (best == null || tip.height > best.height ||
          (tip.height == best.height && tip.hash < best.hash)) {
        best = tip
      }
    })
  })
  return best == null ? null : best.hash
}

// Called after every data update. Plays the sound when the tip moved on to a
// different block: a new block, or a reorg away from the block we knew.
function check_tip_changed() {
  let tip_hash = current_tip_hash()
  if (tip_hash == null) return
  if (state_sound_enabled && state_tip_hash != null && tip_hash != state_tip_hash) {
    console.debug("chain tip changed from", state_tip_hash, "to", tip_hash)
    play_sound()
  }
  state_tip_hash = tip_hash
}

function set_initial_sound() {
  console.debug("called set_initial_sound()")
  // Some browsers restore the checkbox across a reload, but audio can only start
  // after a user gesture, so a restored checkmark would promise a sound we can't
  // play. Start unchecked instead, so the checkbox always reflects reality.
  soundCheckbox.property("checked", false)
  state_sound_enabled = false
}

soundCheckbox.on("input", function() {
  state_sound_enabled = this.checked
  // play the sound once on enabling, so it's clear what to listen for
  if (state_sound_enabled) play_sound()
})

networkSelect.on("input", async function() {
  state_selected_network_id = networkSelect.node().value
  update_network()
  // the tree on screen is the old network's; show the loading overlay so it isn't
  // mistaken for the new one while it loads. draw() hides it again once the new
  // network's tips are drawn (its tip differs from the old network's, so this
  // always triggers the "follow" branch that clears the overlay).
  show_viz_loading()
  // snap straight to the new network's tip instead of panning there: the old
  // network's camera position has nothing to do with the new one, so animating
  // between them would just be a meaningless camera swoop across empty space -
  // and one that would keep going after the loading overlay has already faded.
  await update({ snap: true })
})

async function update(opts) {
  opts = opts || {}
  console.debug("called update()")
  await fetch_data()
  check_tip_changed()
  await draw_nodes()
  await draw({ reason: "update", snap: !!opts.snap })
}

async function run() {
  console.debug("called run()")
  set_initial_sound()
  await fetch_networks()
  await fetch_info()
  await update()

  periodicallyRedrawTimestamps()
}

function periodicallyRedrawTimestamps() {
  setTimeout(() => {
    let ts = document.getElementsByClassName("relativeTimestamp");
    for(t of ts) {
      let timestamp = parseInt(t.dataset.timestamp)
      t.innerHTML = ago(timestamp)
    }
    periodicallyRedrawTimestamps()
  }, 10000)
}

changeSSE.addEventListener('open', () => {
  connectionStatus.style("color", "var(--tip-status-color-active)");
  connectionStatus.attr("title", "connected — receiving live updates");
});

changeSSE.addEventListener('error', (e) => {
  console.error("SSE error", e);
  connectionStatus.style("color", "var(--tip-status-color-invalid)");
  connectionStatus.attr("title", "disconnected — reconnecting…");
});

changeSSE.addEventListener('close', (e) => {
  connectionStatus.style("color", "grey");
  connectionStatus.attr("title", "connection closed");
});

// copy text to the clipboard and confirm with a short toast
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

// Escape closes any open block info boxes
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    closeAllDescriptions()
  }
})

// When a new block is found every node reports it within a moment of the others, so
// the events arrive in a burst. Fetch on the first one immediately - waiting only
// delays showing the block - and then hold off for this long, fetching once more at
// the end if more events came in, so the later nodes' tips aren't missed.
const UPDATE_COOLDOWN_MS = 500

changeSSE.addEventListener("cache_changed", (e) => {
  let data = JSON.parse(e.data)
  console.debug("server side event: the data for one of the networks changed: ", data)
  if(data.network_id != state_selected_network_id) return
  console.debug("server side event: the data for current network changed: ", data)

  if (update_cooling_down) {
    // an update ran just now; remember to run one more once the window is over
    update_scheduled = true
    console.debug("server side event: update for the current network already sheduled: ", data)
    return
  }
  update_cooling_down = true
  update()
  setTimeout(() => {
    update_cooling_down = false
    if (update_scheduled) {
      update_scheduled = false
      update()
    }
  }, UPDATE_COOLDOWN_MS)
})


run()

// ---------------------------------------------------------------------------
// Stratum jobs feed: show which blocks pools are currently mining on top of.
// Off by default; enable for testing by adding ?mining to the URL.
// ---------------------------------------------------------------------------

const STRATUM_SSE_URL = "https://stream.stratum.work/"
// forget a pool entirely if we haven't heard a job from it within this window, so
// pools that stop sending (or disappear from the feed) don't linger forever.
const STRATUM_JOB_TTL_MS = 120000
// pool_name -> { prev_hash, height, last_seen }: the one block each pool is currently
// mining on, and the height it is mining at (= that block's height + 1). Read by
// build_mining_headers() in blocktree.js on every draw, which groups it the other way
// round, by the block being mined on.
var state_stratum_jobs = new Map()
let stratum_redraw_scheduled = false

// the feed's prev_hash lists the header's 4-byte words in header (little-endian)
// order; reversing the word order gives the big-endian display hash used
// everywhere else in the app (header_infos[].hash), so to-be-mined blocks
// resolve against the real tree.
function stratum_prevhash_to_display(hex) {
  let words = []
  for (let i = 0; i < hex.length; i += 8) words.push(hex.slice(i, i + 8))
  return words.reverse().join("")
}

// A pool mines on exactly one block at a time, so a new job replaces whatever we knew
// about that pool. Keying the state by pool (rather than by the block being mined on)
// is what makes that replacement automatic: keyed the other way round, a pool that
// switched to a new block would keep haunting the old one until its TTL ran out, and
// look like it was mining two blocks at once.
// Returns true when the job moves this pool onto a block no other pool was mining on,
// i.e. when it changes which blocks are drawn rather than just who is on them.
function record_stratum_job(job) {
  if (job == null || !job.prev_hash || !job.pool_name) return false
  let prev_hash = stratum_prevhash_to_display(job.prev_hash)
  let known = false
  state_stratum_jobs.forEach(other => { if (other.prev_hash == prev_hash) known = true })
  state_stratum_jobs.set(job.pool_name, {
    prev_hash: prev_hash,
    height: job.height,
    last_seen: Date.now(),
  })
  return !known
}

// how long to coalesce jobs that only shuffle pools between blocks we already draw
const STRATUM_REDRAW_COALESCE_MS = 300

// Jobs arrive several times a second, so they are coalesced into at most one redraw
// per window (and never recenter the viewport). The exception is a job that puts a
// pool on a block we aren't drawing yet - the first pool to switch after a new block
// is found - which redraws straight away, since that is the moment the display is
// out of date and worth updating. refresh_mining() only does a full redraw when the
// set of being-mined blocks changes; otherwise it just re-lays-out the pool cloud,
// leaving the to-be-mined blocks (and their pulse animation) untouched.
function schedule_stratum_redraw(immediate) {
  if (immediate) {
    refresh_mining()
    return
  }
  if (stratum_redraw_scheduled) return
  stratum_redraw_scheduled = true
  setTimeout(() => {
    stratum_redraw_scheduled = false
    refresh_mining()
  }, STRATUM_REDRAW_COALESCE_MS)
}

let stratumSource = null
function connect_stratum() {
  try {
    // EventSource reconnects on its own after an error (with the server's
    // requested retry delay, or a browser default), so no manual backoff here.
    stratumSource = new EventSource(STRATUM_SSE_URL)
  } catch (e) {
    console.error("could not open stratum jobs stream", e)
    return
  }
  stratumSource.addEventListener("message", (e) => {
    let job
    try { job = JSON.parse(e.data) } catch (_) { return }
    let new_block = record_stratum_job(job)
    schedule_stratum_redraw(new_block)
  })
  stratumSource.addEventListener("error", (e) => {
    console.debug("stratum jobs stream error, browser will retry", e)
  })
}

// only connect (and thus show any being-mined blocks) when opted in via ?mining
if (MINING_ENABLED) {
  connect_stratum()
} else {
  console.debug("mining jobs feed disabled; add ?mining to the URL to enable it")
}
