const NODE_SIZE = 120
const MAX_USIZE = 18446744073709551615;
const BLOCK_SIZE = 50
const BLOCK_DEPTH = 9 // depth of the 3D extrusion (offset toward the top-right)
const MIN_DIFFICULTY = 1

const orientationSelect = d3.select("#orientation")

const orientations = {
  "bottom-to-top": {
    name: "bottom-to-top",
    x: (d, _) => d.x,
    y: (d, htoi) => -htoi[d.data.data.height] * NODE_SIZE,
    // start the link half a depth beyond the parent's top edge, over the middle of
    // its depth face, so it looks like it comes from the center of the 3D block.
    linkDir: (htoi) => d3.linkVertical()
      .source(l => [o.x(l.source, htoi) + BLOCK_DEPTH/2, o.y(l.source, htoi) - BLOCK_SIZE/2 - BLOCK_DEPTH/2])
      .target(l => [o.x(l.target, htoi) + BLOCK_DEPTH/2, o.y(l.target, htoi) - BLOCK_DEPTH/2 - 20]),
    hidden_blocks_text: {offset_x: -15, offset_y: 0, anchor: "left"},
    block_text_rotate: -90,
    miner_dy: BLOCK_SIZE * (3/4),
    // To make the miner label look centered on the link between the two blocks,
    // we move it half of BLOCK_DEPTH to the right.
    miner_dx: BLOCK_DEPTH / 2,
    // where the tip block should land in the viewport on the initial draw: the chain
    // grows downward, so put the tip near the top to show less empty space.
    tip_anchor: (w, h) => [w/2, h*0.25],
    // one block slot further along the chain, i.e. where the next block will appear
    next_slot: {x: 0, y: -NODE_SIZE},
    // the along-chain axis is y (growing negative), so the countdown marker is a
    // horizontal line at a fixed y spanning the cross axis (x).
    countdown_along: (idx) => -idx * NODE_SIZE,
    countdown_line: (along, cross_min, cross_max) => ({x1: cross_min, y1: along, x2: cross_max, y2: along}),
    countdown_label_pos: (along, cross_min) => ({x: cross_min, y: along}),
  },
  "left-to-right": {
    name: "left-to-right",
    x: (d, htoi) => htoi[d.data.data.height] * NODE_SIZE,
    y: (d, _) => d.x,
    // start the link half a depth beyond the parent's right edge, over the middle of
    // its depth face, so it looks like it comes from the center of the 3D block.
    linkDir: (htoi) => d3.linkHorizontal()
      .source(l => [o.x(l.source, htoi) + BLOCK_SIZE/2 + BLOCK_DEPTH/2, o.y(l.source, htoi) - BLOCK_DEPTH/2])
      .target(l => [o.x(l.target, htoi), o.y(l.target, htoi) - BLOCK_DEPTH/2]),
    hidden_blocks_text: {offset_x: 0, offset_y: 15, anchor: "middle"},
    block_text_rotate: 0,
    // the label lands above the block, over the top depth face, so it must clear it.
    miner_dy: BLOCK_SIZE * (3/4),
    miner_dx: 0,
    // where the tip block should land in the viewport on the initial draw: the chain
    // grows rightward, so put the tip near the right to show less empty space.
    tip_anchor: (w, h) => [w*0.75, h/2],
    // one block slot further along the chain, i.e. where the next block will appear
    next_slot: {x: NODE_SIZE, y: 0},
    // the along-chain axis is x, so the countdown marker is a vertical line at a
    // fixed x spanning the cross axis (y).
    countdown_along: (idx) => idx * NODE_SIZE,
    countdown_line: (along, cross_min, cross_max) => ({x1: along, y1: cross_min, x2: along, y2: cross_max}),
    countdown_label_pos: (along, cross_min) => ({x: along, y: cross_min}),
  },
};

// A countdown marker is only drawn once the chain tip is within this many
// blocks of the configured countdown height.
const COUNTDOWN_MARKER_THRESHOLD = 10

const status_to_color = {
  "active": "lime",
  "invalid": "fuchsia",
  "valid-fork": "cyan",
  "valid-headers": "red",
  "headers-only": "yellow",
}

// tip info label: a stack of colored "Nx status" boxes shown next to each tip block,
// rotated like the miner text so it sits opposite it.
const TIP_BOX_H = 10      // height of one status box
const TIP_PAD_X = 2       // horizontal padding inside a box
const TIP_ROW_GAP = 2     // gap between stacked boxes
// order the boxes by status so they always appear in the same sequence
const status_order = {
  "active": 0,
  "valid-fork": 1,
  "valid-headers": 2,
  "headers-only": 3,
  "invalid": 4,
}

// BIP 9 signalling. A block signals for a deployment when the top three bits of its
// version are 001 and the deployment's bit is set. Versions outside that range carry
// no signal however their bits happen to fall — the legacy version 1 to 4 blocks
// would otherwise read as signalling for bits 0, 1 and 2.
//
// Bits are reused between deployments: bit 4 carried BIP 91 in July 2017 before
// BIP-110 took it. That is not worth guarding against here, since fork-observer only
// ever draws the recent tip of the chain, never 2017.
const VERSIONBITS_TOP_MASK = 0xe0000000
const VERSIONBITS_TOP_BITS = 0x20000000

// Deployments that get a chip on the block, keyed by the version bit they use. `chip`
// has to fit on a 50px block face, so it stays short; `name` goes in the block info
// card and `title` in the chip's tooltip.
const SIGNALLING_DEPLOYMENTS = {
  4: {
    chip: "BIP-110",
    name: "BIP-110 (bit 4)",
    title: "Signals for BIP-110 — Reduced Data Temporary Softfork — on version bit 4.",
  },
}

// signalling chip: a small box on the block face, one per deployment being signalled
const CHIP_H = 9        // height of one chip
const CHIP_PAD_X = 2    // horizontal padding inside a chip
const CHIP_GAP = 2      // gap between stacked chips
const CHIP_INSET = 3    // distance from the block's bottom-left corner

// the deployments a block signals for, in bit order. Empty for the vast majority of
// blocks, which is what the chip's presence is meant to stand out against.
function signalled_deployments(header) {
  if (((header.version & VERSIONBITS_TOP_MASK) >>> 0) != VERSIONBITS_TOP_BITS) return []
  return Object.keys(SIGNALLING_DEPLOYMENTS)
    .map(Number)
    .sort(d3.ascending)
    .filter(bit => (header.version & (1 << bit)) != 0)
    .map(bit => SIGNALLING_DEPLOYMENTS[bit])
}

let o = orientations["left-to-right"];

// absolute position of the current tip, remembered so the "recenter" button can
// bring the view back to it after the user pans/zooms away
let lastTipPos = { x: 0, y: 0 }

// what the camera is currently anchored on: the tip hash it points at, plus the
// orientation that tip was laid out in. Null before the first draw. This is
// deliberately not the same as lastTipPos: the camera only follows when there is
// genuinely something else to look at, not on every change of the point we focus on.
// See the end of draw().
let viewAnchor = null

// Blocks that come from the stratum jobs feed rather than from our own nodes: the one
// being mined, and the one that was just found and that pools are already building on.
// Both are drawn the same way - solid and pulsing - and only their tag tells them
// apart. Drawing them solid also keeps the links from showing through the block.
// The keys are the statuses build_mining_headers() gives those blocks.
const MINING_TAGS = {
  "mining": {
    text: "being mined",
    title: () => "Pools are working on this block right now, according to the stratum jobs feed.",
  },
  "just-found": {
    text: "just found",
    title: d => "Pools are already mining on top of block " + d.data.data.real_hash +
      ", so it exists — but none of our nodes have reported it yet.",
  },
}

function from_stratum_feed(header) {
  return header.status in MINING_TAGS
}

// size a group's background rect to fit the text next to it, with `px`/`py` of padding.
// Both are only measurable once the text has been laid out, hence the getBBox().
function fit_rect_to_text(group, px, py) {
  let bb = group.select("text").node().getBBox()
  group.select("rect")
    .attr("x", bb.x - px).attr("y", bb.y - py)
    .attr("width", bb.width + 2 * px).attr("height", bb.height + 2 * py)
}

// TEMPORARY: logging for the "the view keeps moving" bug. Two different things read as
// the view moving: the camera panning, and the blocks sliding underneath a camera that
// stayed put. Both are logged, so a jump can be attributed to one or the other.
// Remove this (and its call sites) once the bug is understood.
const VIEW_DEBUG = true
function log_view(what, info) {
  if (VIEW_DEBUG) console.log("[view] " + what, info)
}

// positions of the real blocks at the last draw, to detect the layout shifting under
// the camera. Part of the TEMPORARY logging above.
let lastBlockPos = new Map()
function log_layout_shift(root_node, htoi) {
  if (!VIEW_DEBUG) return
  let now = new Map(), moved = [], max = 0
  root_node.descendants().forEach(d => {
    let key = d.data.data.hash
    let pos = [o.x(d, htoi), o.y(d, htoi)]
    now.set(key, pos)
    let was = lastBlockPos.get(key)
    if (was === undefined) return
    let dx = pos[0] - was[0], dy = pos[1] - was[1]
    if (dx || dy) {
      moved.push({ h: d.data.data.height, status: d.data.data.status, dx, dy })
      max = Math.max(max, Math.abs(dx), Math.abs(dy))
    }
  })
  lastBlockPos = now
  if (moved.length) log_view("layout shifted", { count: moved.length, max, moved: moved.slice(0, 8) })
}

let svg = d3
    .select("#drawing-area")

let initialDraw = true

// enables zoom and panning
const zoom = d3.zoom().scaleExtent([0.15, 5])
  // interpolate transitions in a straight line. d3's default (interpolateZoom) flies
  // the camera along an arc that zooms out and back in again, which for the short
  // pans we do here reads as the view lurching away and returning.
  .interpolate(d3.interpolate)
  .on( "zoom", e => {
  g.attr("transform", e.transform)
  // re-measure the text-fitted boxes; their metrics can be stale if they were first
  // sized before the text was fully laid out
  recalc_miner_boxes()
  recalc_tip_boxes()
  recalc_signal_chips()
})
svg.call(zoom)

let g = svg
    .append("g")

// layer for the connector links between a block and its open description. It is
// never raised, so it stays below the blocks and the lines appear to originate
// from underneath the block they belong to.
let connectorLayer = g
    .append("g")
    .attr("id", "description-connectors")

// layer for the connector lines from a being-mined block to its pool labels. Like
// connectorLayer it is never raised, so the lines stay below the blocks and appear to
// come out from behind the to-be-mined block.
let miningLinkLayer = g
    .append("g")
    .attr("id", "mining-links")

// layer for the force-positioned pool-name labels around "being mined" blocks. It is
// raised above the blocks on every draw so the labels stay readable.
let miningLabelLayer = g
    .append("g")
    .attr("id", "mining-labels")

// overlay layer that always holds the open block descriptions (info boxes). It is
// raised to the top on every draw so the boxes are never painted over by blocks or
// tip status markers.
let descLayer = g
    .append("g")
    .attr("id", "descriptions")

// layer holding the countdown marker line + label, redrawn from scratch on every
// draw() call (cheap: at most one line and one text).
let countdownLayer = g
    .append("g")
    .attr("id", "countdown")

// context from the last draw, so job updates can refresh just the pool cloud (and
// detect whether a full redraw is actually needed) without re-rendering the blocks.
let miningDrawCtx = null

// invert the current stratum jobs (state_stratum_jobs, one entry per pool, kept up to
// date in main.js) into a map of prev_hash -> { parent, just_found, pool_names }: the
// pools mining on each block. Expired pools are pruned here so the set stays current
// without a separate timer.
//
// `parent` is the header the to-be-mined block hangs off. Normally that is the block
// the pools name in their job. When we don't know that block it is our tip instead, and
// `just_found` says a stand-in for the block the feed told us about goes in between:
// that happens for a few seconds after every block, because pools hear about it through
// their own infrastructure well before a node has received, validated and relayed it.
function current_mining_by_prev(header_infos, max_height) {
  let result = new Map()
  if (!MINING_ENABLED || state_stratum_jobs.size === 0) {
    return result
  }
  const now = Date.now()
  // index the real headers by hash so we can resolve a job's parent block
  let by_hash = new Map()
  header_infos.forEach(h => by_hash.set(h.hash, h))
  // the block every job builds on sits one below the height being mined
  let tip = header_infos.filter(h => h.height == max_height)[0]

  state_stratum_jobs.forEach((job, pool_name) => {
    // drop pools we haven't heard from in a while
    if (now - job.last_seen > STRATUM_JOB_TTL_MS) {
      state_stratum_jobs.delete(pool_name)
      return
    }
    let parent = by_hash.get(job.prev_hash)
    // A parent we don't know is only worth drawing when the feed places it exactly one
    // block past our tip, i.e. it is the block that was just found. Anything further
    // ahead (or behind) would mean speculating about a chain we know nothing about, so
    // those jobs are dropped.
    let just_found = parent === undefined && tip !== undefined && job.height == max_height + 2
    if (parent === undefined && !just_found) return
    // the pair then hangs off our tip, with the stand-in block in between
    if (just_found) parent = tip

    let entry = result.get(job.prev_hash)
    if (entry === undefined) {
      entry = { parent, just_found, pool_names: [] }
      result.set(job.prev_hash, entry)
    }
    entry.pool_names.push(pool_name)
  })
  // stable order, so the labels don't get reshuffled between refreshes
  result.forEach(entry => entry.pool_names.sort())
  return result
}

// build synthetic header objects to inject into the tree: one to-be-mined block per
// prev_hash, aggregating all pools mining on it, plus - for pools that have already
// moved on to a block we haven't seen - a "just found" block standing in for it, so
// the to-be-mined block has something to hang off.
function build_mining_headers(header_infos, max_height) {
  let mining_headers = []
  current_mining_by_prev(header_infos, max_height)
    .forEach(({ parent, just_found, pool_names }, prev_hash) => {
      if (just_found) {
        // The feed gave us this block's real hash, kept in real_hash. The `hash` used
        // for identity is prefixed so it can't collide with the real header once a node
        // reports it: the blocks are keyed by hash, and sharing a key would morph this
        // one into the real block while leaving its "not seen yet" styling behind.
        // With a distinct key it is simply removed and the real block drawn instead.
        mining_headers.push({
          id: "found-" + prev_hash,
          prev_id: parent.id,
          height: parent.height + 1,
          hash: "found-" + prev_hash,
          real_hash: prev_hash,
          prev_blockhash: parent.hash,
          miner: "",
          difficulty_int: 0,
          status: "just-found",
        })
      }
      mining_headers.push({
        id: "mining-" + prev_hash,
        prev_id: just_found ? "found-" + prev_hash : parent.id,
        height: parent.height + (just_found ? 2 : 1),
        hash: "mining-" + prev_hash,
        prev_blockhash: prev_hash,
        // pool names are shown in a force-positioned cloud around the block, so the
        // block itself carries no miner label
        miner: "",
        // not MIN_DIFFICULTY, so it doesn't get the accent-colored stroke
        difficulty_int: 0,
        status: "mining",
        mining_pools: pool_names,
      })
    })
  return mining_headers
}

// called when new stratum jobs arrive. If the set of being-mined blocks is unchanged
// (the common case: same tip, just a shifting pool set) only the pool cloud is
// refreshed, leaving the block DOM — and its running pulse animation — untouched. A
// full redraw happens only when a to-be-mined block appears or disappears.
function refresh_mining() {
  // jobs can arrive before the first fetch has returned; there is nothing to draw yet
  if (!state_data.header_infos || state_data.header_infos.length == 0) return
  if (miningDrawCtx === null) {
    draw({ preserveView: true, reason: "jobs (first draw)" })
    return
  }
  let desired = current_mining_by_prev(state_data.header_infos, miningDrawCtx.max_height)
  let current_keys = miningDrawCtx.toBeMinedByPrev
  let same = desired.size === current_keys.size &&
    Array.from(desired.keys()).every(k => current_keys.has(k))
  if (!same) {
    log_view("to-be-mined set changed", {
      was: Array.from(current_keys.keys()).map(k => k.substring(56)),
      now: Array.from(desired.keys()).map(k => k.substring(56)),
    })
    draw({ preserveView: true, reason: "jobs (block set changed)" })
    return
  }
  // same set of blocks: update their pool lists in place and re-lay-out the cloud only
  desired.forEach(({ pool_names }, prev_hash) => {
    let node = current_keys.get(prev_hash)
    if (node) node.data.data.mining_pools = pool_names
  })
  draw_mining_pool_clouds(miningDrawCtx.root_node, miningDrawCtx.htoi)
}

function preprocess_data(data) {
  let header_infos = data.header_infos;
  let node_infos = data.nodes;

  let hash_to_tipstatus = {}
  node_infos.forEach(node => {
    node.tips.forEach(tip => {
      if (!(tip.hash in hash_to_tipstatus)) {
        hash_to_tipstatus[tip.hash] = {}
      }
      if (!(tip.status in hash_to_tipstatus[tip.hash])) {
        hash_to_tipstatus[tip.hash][tip.status] = { status: tip.status, count: 0, nodes: []  }
      }
      hash_to_tipstatus[tip.hash][tip.status].count++
      hash_to_tipstatus[tip.hash][tip.status].nodes.push(node)
    });
  });

  header_infos.forEach(header_info => {
    let status = hash_to_tipstatus[header_info.hash];
    header_info.status = status == undefined? "in-chain" : Object.values(status)
    header_info.is_tip = status != undefined
  })

  // synthetic blocks from the stratum jobs feed, injected as children of the block
  // each pool builds on. max_height stays based on the real headers only, so these are
  // not treated as the animated newest block.
  const max_height = Math.max(...header_infos.map(d => d.height))
  let mining_headers = build_mining_headers(header_infos, max_height)

  var treeData = d3
    .stratify()
    .id(d => d.id)
    .parentId(function (d) {
      // d3js requires the first prev block hash to be null
      return (d.prev_id == MAX_USIZE ? null : d.prev_id)
    })(header_infos.concat(mining_headers));

  stripUninteresting(treeData, 4)

  let interesting_heights = []
  treeData.descendants().forEach(d => {
    interesting_heights.push(d.data.height)
    // This adds extra spacing in a collapsed chain.
    interesting_heights.push(d.data.height + 1)
  })

  let unique_heights = Array.from(new Set(interesting_heights));
  unique_heights.sort((a, b) => (a - b))

  let htoi = {}; // height to array index map
  let last_height = 0;
  let index = 0;
  unique_heights.forEach( height => {
    if (last_height + 1 > height) {
      index +=1;
    }
    htoi[height] = index;
    index += 1;
    last_height = height;
  });

  let treemap = gen_treemap();

  // Make sure the headers and forks are sorted deterministically. This means, they
  // don't change on redraws, which is nicer.
  const sort_blocks = (a, b) =>
    d3.ascending(a.data.data.height, b.data.data.height) ||
    d3.ascending(a.data.data.hash, b.data.data.hash)

  // assigns the data to a hierarchy using parent-child relationships
  // and maps the node data to the tree layout.
  const root_node = treemap(d3.hierarchy(treeData).sort(sort_blocks))

  // Where the real blocks end up across the chain comes from a second run of the same
  // layout with the feed's synthetic blocks left out. Laying both out together lets the
  // feed move the real chain around: a to-be-mined block makes the branch it hangs off
  // one deeper, and d3.tree answers that by pushing the neighbouring branches apart.
  // Pools switch jobs several times a second, so that showed up as the whole tree
  // sliding back and forth for no visible reason. Both runs walk the same stripped
  // tree, so every real block is in both and htoi above stays valid for either.
  //
  // Only the feed blocks ahead of our tip are left out that way. One at or below it
  // competes with a block we already have, so it is a fork and is laid out with the
  // real blocks - which separates it from its sibling like any other fork, instead of
  // leaving it half a slot away and drawn over it. That moves the chain, but so would
  // the confirmed block it stands in for.
  const beyond_our_tip = d => from_stratum_feed(d.data) && d.data.height > max_height
  const real_root = treemap(
    d3.hierarchy(treeData, d => (d.children || []).filter(c => !beyond_our_tip(c)))
      .sort(sort_blocks))
  let real_x = new Map()
  real_root.descendants().forEach(d => real_x.set(d.data.data.hash, d.x))

  // Pin every real block back to where it sits without the feed. The synthetic ones
  // ahead of the tip aren't in that layout, so they take the shift of the block they
  // hang off - which keeps them lined up with it and keeps any siblings as far apart
  // as they were.
  // eachBefore visits parents first, so a node's shift is always known by then.
  let shift = new Map()
  root_node.eachBefore(d => {
    let real = real_x.get(d.data.data.hash)
    let s = real !== undefined ? real - d.x : shift.get(d.parent)
    shift.set(d, s)
    d.x += s
  })

  return [root_node, max_height, htoi]
}

function draw(opts) {
  opts = opts || {}
  let data = state_data

  // nothing to draw if there are no headers
  if (data.header_infos.length == 0) {
    return
  }

  const [root_node, max_height, htoi] = preprocess_data(data)

  log_layout_shift(root_node, htoi)

  // An orientation switch moves every block into a different coordinate space, tens of
  // thousands of pixels away. Animating that flies the camera - and slides the blocks -
  // through empty space for the better part of a second, which just reads as the view
  // having gone blank. So a switch snaps into place instead, the way the first draw
  // does; there is no continuity between the two layouts to preserve anyway.
  const snap = initialDraw || !!opts.snap
  const move = (sel, ms) => snap ? sel : sel.transition(d3.transition().duration(ms))

  // the 3D extrusion (top + right faces) lives in its own layer below the links, so
  // a link can pass over a block's depth and tuck behind its front face — making it
  // look like it comes from the center of the block.
  let backFaces = g
    .selectAll(".block-back")
    .data(root_node.descendants(), d => `${d.data.data.hash}-${d.data.data.height}`)
    .join(
      enter => {
        let back = enter.append("g")
          .attr("class", "block-back")
          .classed("being-mined", d => from_stratum_feed(d.data.data))
          .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")
        const half = BLOCK_SIZE / 2
        const DEPTH = BLOCK_DEPTH
        back.append("polygon")
          .attr("class", "block-face-top")
          .attr("points", `${-half},${-half} ${half},${-half} ${half + DEPTH},${-half - DEPTH} ${-half + DEPTH},${-half - DEPTH}`)
        back.append("polygon")
          .attr("class", "block-face-side")
          .attr("points", `${half},${-half} ${half + DEPTH},${-half - DEPTH} ${half + DEPTH},${half - DEPTH} ${half},${half}`)
        // edges of the top/side/back faces, each drawn once (the front square's edges
        // come from the front rect's own stroke, so nothing overlaps)
        back.append("path")
          .attr("class", "block-edges")
          .attr("d", `M ${-half},${-half} L ${-half + DEPTH},${-half - DEPTH} L ${half + DEPTH},${-half - DEPTH} L ${half},${-half}`
            + ` M ${half + DEPTH},${-half - DEPTH} L ${half + DEPTH},${half - DEPTH} L ${half},${half}`)
        return back
      },
      update => {
        move(update, 600)
          .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")
        return update
      }
    )

  let links = g
    .selectAll(".link-block-block")
    .data(root_node.links(), d => `${d.source.data.data.hash}-${d.target.data.data.hash}`)
    .join(
      enter => {
        enter.append("path")
          .attr("class", "link link-block-block")
          .attr("filter", "#url(shadow)")
          .attr("d", o.linkDir(htoi))
          .attr("stroke-dasharray", (d, x, y) => d.target.data.data.height - d.source.data.data.height == 1 ? y[x].getTotalLength() + " "  + y[x].getTotalLength() : "4 5")
          .attr("fill", "transparent")
          .attr("stroke-dashoffset", (d, x, y) => y[x].getTotalLength())
          .attr("stroke-opacity", 1)
          .classed("being-mined", d => from_stratum_feed(d.target.data.data))
          .transition(d3.transition().duration(300))
          .attr("stroke-dashoffset", 0)
          .attr("stroke-opacity", 0.2)
          .transition(d3.transition().duration(300))
          .attr("stroke-opacity", 1)
      },
      update => {
        move(update, 600)
          .attr("d", o.linkDir(htoi))
          .attr("stroke-dasharray", (d, x, y) => d.target.data.data.height - d.source.data.data.height == 1 ? y[x].getTotalLength() + " "  + y[x].getTotalLength() : "4 5")
          .attr("stroke-dashoffset", 0)
          .attr("stroke-opacity", 1)
      }
    )

  let hiddenBlockTexts = g
    .selectAll(".text-blocks-not-shown")
    .data(root_node.links().filter(d => d.target.data.data.height - d.source.data.data.height != 1), d => d.source.data.data.hash + d.target.data.data.hash)
    .join(
      enter => {
        let blocksNotShown = enter.append("text")
          .attr("class", "text-blocks-not-shown")
          .style("text-anchor", o.hidden_blocks_text.anchor)
          .style("font-size", "12px")
          .attr("x", d => hidden_text_x(d, htoi))
          .attr("y", d => hidden_text_y(d, htoi))
          .attr("transform", d => `rotate(${o.block_text_rotate}, ${hidden_text_x(d, htoi)},${hidden_text_y(d, htoi)})`)

        blocksNotShown.append("tspan")
          .text(d => (d.target.data.data.height - d.source.data.data.height -1) + " blocks hidden")
          .attr("dy", ".3em")
        return blocksNotShown
      },
      update => {
        move(update, 600)
          .attr("x", d => hidden_text_x(d, htoi))
          .attr("y", d => hidden_text_y(d, htoi))
          .attr("transform", d => `rotate(${o.block_text_rotate}, ${hidden_text_x(d, htoi)},${hidden_text_y(d, htoi)})`)
      }
    )

  // adds each block as a group
  let blocks = g
    .selectAll(".block")
    .data(root_node.descendants(), d => `${d.data.data.hash}-${d.data.data.height}`)
    .join(
      enter => {
        let newBlocks = enter.append("g")
          .classed("block", true)
          .classed("being-mined", d => from_stratum_feed(d.data.data))
          .attr("id", d => "block-" + d.data.data.height + "-" + d.data.data.hash)
          .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")
          .attr("x", d => o.x(d, htoi))
          .attr("y", d => o.y(d, htoi))
          .on("click", (c, d) => onBlockClick(c, d))

        let block_child_group = newBlocks.append("g")
          .attr("class", "block-child-group")

        let block_backgrounds = block_child_group.insert("rect")
          .attr("class", "block-background")
          .attr("stroke", d => d.data.data.difficulty_int == MIN_DIFFICULTY ? "var(--accent)" : "var(--block-stroke)")
          .attr("stroke-width", d => d.data.data.difficulty_int == MIN_DIFFICULTY ? 3 : 1)
          .attr("stroke-linejoin", "round")
          .attr("stroke-opacity", 1)
          .classed("being-mined", d => from_stratum_feed(d.data.data))

        block_backgrounds.filter(d => d.data.data.height != max_height || initialDraw)
          .attr("x", -BLOCK_SIZE/2)
          .attr("y", -BLOCK_SIZE/2)
          .attr("height", d => BLOCK_SIZE)
          .attr("width", d => BLOCK_SIZE)

        let height_text = block_child_group
          .insert("text")
          .attr("dy", ".35em")
          .attr("class", "block-text")
          .text(d => d.data.data.height);

        // miner tag: a small background box (rect) behind the miner text. the group
        // carries the rotation; the rect is sized to the text in a later layout pass.
        let miner_group = block_child_group
          .append("g")
          .attr("class", "block-miner-group")
        miner_group.append("rect").attr("class", "block-miner-bg")
        let pool_text = miner_group
          .append("text")
          .classed("block-pool-name", true)
          .attr("dy", o.miner_dy)
          .attr("dx", o.miner_dx)
          .classed("block-miner", true)
          .text(d => d.data.data.miner.length > 14 ? d.data.data.miner.substring(0, 14) + "…" : d.data.data.miner);

        // status tag below the blocks that come from the stratum feed, styled like the
        // tip-status boxes so it reads as a status label. Lives in the block group, so
        // it persists (and isn't re-rendered) on the frequent pool-only refreshes.
        let mining_tag = block_child_group.filter(d => d.data.data.status in MINING_TAGS)
          .append("g")
          .attr("class", d => "mining-tag mining-tag-" + d.data.data.status)
          .attr("transform", "translate(" + -BLOCK_SIZE/2 + "," + (+(BLOCK_SIZE / 2) + BLOCK_DEPTH) + ")")
        mining_tag.append("rect").attr("class", "mining-tag-bg")
        mining_tag.append("text").attr("class", "mining-tag-text")
          .attr("text-anchor", "left").attr("dy", ".35em")
          .text(d => MINING_TAGS[d.data.data.status].text)
          .append("title").text(d => MINING_TAGS[d.data.data.status].title(d))
        mining_tag.each(function () { fit_rect_to_text(d3.select(this), 1, 1) })

        // signalling chips, stacked in the bottom-left of the block face — the one
        // corner nothing else uses, and inside the block so they need no per-
        // orientation placement. The container is bound to the block (not to a
        // deployment) so it can be faded in with the rest of a new block below; it
        // stays empty for the blocks that signal for nothing.
        let signal_group = block_child_group.append("g").attr("class", "signal-chips")
        signal_group.selectAll("g.signal-chip")
          .data(d => signalled_deployments(d.data.data), d => d.chip)
          .join(enter => {
            let chip = enter.append("g").attr("class", "signal-chip")
            chip.append("rect").attr("class", "signal-chip-bg")
            chip.append("text").attr("class", "signal-chip-text").attr("dy", ".35em").text(d => d.chip)
            chip.append("title").text(d => d.title)
            return chip
          })

        if (!initialDraw) {
          block_backgrounds
            .filter(d => d.data.data.height == max_height)
            .attr("transform", "scale(0.1)")
            .attr("height", d => BLOCK_SIZE)
            .attr("width", d => BLOCK_SIZE)
            .transition(d3.transition().duration(600))
            .attr("x", -BLOCK_SIZE/2)
            .attr("y", -BLOCK_SIZE/2)
            .attr("transform", "scale(1)")

          miner_group
            .filter(d => d.data.data.height == max_height)
            .style("opacity", 0)
            .transition(d3.transition().duration(600))
            .style("opacity", 1)

          signal_group
            .filter(d => d.data.data.height == max_height)
            .style("opacity", 0)
            .transition(d3.transition().duration(600))
            .style("opacity", 1)

          height_text
            .filter(d => d.data.data.height == max_height)
            .style("font-size", "0px")
            .transition(d3.transition().duration(600))
            .style("font-size", "11px")
        }

        return newBlocks
      },
      update => {
        move(update, 600)
          .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")
        // keep the stored anchor coordinates in sync (info boxes read these), or
        // they'd stay at the previous orientation's position after a switch
        update
          .attr("x", d => o.x(d, htoi))
          .attr("y", d => o.y(d, htoi))
        update.selectAll(".block-pool-name")
          .attr("dy", o.miner_dy)
          .attr("dx", o.miner_dx)

        update.raise()
        return update
      }
    );

  // pool names orbiting each "being mined" block, laid out with a force simulation
  if (opts.clearPoolCloud) {
    clear_mining_pool_clouds()
  } else {
    draw_mining_pool_clouds(root_node, htoi)
  }

  // remember what we drew so incoming jobs can refresh the cloud in place, without a
  // full redraw that would restart the to-be-mined blocks' pulse animation
  let toBeMinedByPrev = new Map()
  root_node.descendants().filter(d => d.data.data.status == "mining")
    .forEach(d => toBeMinedByPrev.set(d.data.data.prev_blockhash, d))
  miningDrawCtx = { root_node, htoi, max_height, toBeMinedByPrev }

  // size the miner background box to fit its (already positioned) text
  recalc_miner_boxes()
  recalc_signal_chips()

  // tip info label: a stack of colored "Nx status" boxes next to each tip block. the
  // whole group is rotated like the miner text so it sits on the opposite side.
  let node_groups = g
    .selectAll(".tip-info")
    .data(root_node.descendants().filter(d => d.data.data.status != "in-chain" && !from_stratum_feed(d.data.data)),
      d => `${d.data.data.hash}-${d.data.data.height}`)
    .join("g")
    .classed("tip-info", true)
    .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")

  // build the box (rect + text + title) once per status on enter, so redraws don't
  // accumulate copies
  let tip_rows = node_groups.selectAll("g.tip-info-row")
    .data(
      d => d.data.data.status.slice().sort((a, b) => status_order[a.status] - status_order[b.status]),
      d => d.status
    )
    .join(enter => {
      let row = enter.append("g").attr("class", "tip-info-row")
      row.append("title")
      row.append("rect").attr("class", "tip-info-bg")
      row.append("text").attr("class", "tip-info-text").attr("text-anchor", "start").attr("dy", ".35em")
      return row
    })

  tip_rows.select("rect").attr("class", d => "tip-info-bg tip-status-color-fill-" + d.status)
  tip_rows.select("text").text(d => d.count + "x " + d.status)
  tip_rows.select("title").text(d => d.nodes.map(node => node.name).join(", "))

  // measure each label and stack the boxes just off the block
  recalc_tip_boxes()

  let offset_x = 0;
  let offset_y = 0;
  // the real tip at max_height. Not necessarily a leaf(): a to-be-mined block is
  // injected as its child, so filter descendants by height instead of using leaves().
  let max_height_tip = root_node.descendants().filter(d => d.data.data.status != "mining" && d.data.data.height == max_height)[0]
  if (max_height_tip !== undefined) {
    offset_x = o.x(max_height_tip, htoi);
    offset_y = o.y(max_height_tip, htoi);
    // With the mining feature on, center one slot past the tip - where the to-be-mined
    // block sits (and where the next block will appear) - so it and its pool cloud are
    // in view. This deliberately depends only on the mode, not on whether a to-be-mined
    // block happens to exist right now: pools switch jobs constantly, so anchoring on
    // the block itself dragged the view back and forth by a block slot as it came and
    // went.
    if (MINING_ENABLED) {
      offset_x += o.next_slot.x;
      offset_y += o.next_slot.y;
    }
  }

  draw_countdown(htoi, max_height, root_node)

  // stack, bottom to top: 3D depth faces, then the links over them, then the block
  // front faces (so links tuck behind the front face and look centered), then the
  // tip status markers
  backFaces.raise()
  g.selectAll(".link-block-block").raise()
  g.selectAll(".text-blocks-not-shown").raise()
  blocks.raise()
  node_groups.raise()
  countdownLayer.raise()
  miningLabelLayer.raise()

  // keep open descriptions (and their connectors) anchored to their block as the
  // layout shifts, and raise the overlay so the info boxes stay on top of everything
  descLayer.selectAll(".block-description").each(function () {
    let hash = this.getAttribute("data-hash")
    let node = root_node.descendants().find(n => n.data.data.hash == hash)
    let connector = connectorLayer.selectAll(".link-block-description")
      .filter(function () { return this.getAttribute("data-hash") == hash })
    if (node === undefined) {
      // the block this description belonged to is gone
      d3.select(this).remove()
      connector.remove()
    } else {
      let transform = "translate(" + o.x(node, htoi) + "," + o.y(node, htoi) + ")"
      d3.select(this).attr("transform", transform)
      connector.attr("transform", transform)
    }
  })
  descLayer.raise()

  lastTipPos = { x: offset_x, y: offset_y }

  // Panning the camera is disruptive, so only do it when there is genuinely something
  // else to look at: a new tip, a reorg, or a switch of orientation - which moves every
  // block into a different coordinate space and would otherwise leave the camera
  // pointing at empty space.
  let tip_hash = max_height_tip === undefined ? null : max_height_tip.data.data.hash
  let anchor = tip_hash + "|" + o.name
  let follow = initialDraw || viewAnchor === null || viewAnchor != anchor

  log_view("draw", {
    reason: opts.reason || "?",
    preserveView: !!opts.preserveView,
    orientation: o.name,
    tip: tip_hash && tip_hash.substring(56),
    max_height,
    offset: [offset_x, offset_y],
    viewAnchor, anchor, follow,
    willMoveCamera: !opts.preserveView && follow,
  })

  // job-triggered redraws (new stratum jobs arriving) pass preserveView so the
  // viewport isn't yanked back to the tip while the user is panning around.
  if (!opts.preserveView && follow) {
    viewAnchor = anchor
    zoom.scaleBy(svg, 1);
    let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
    zoom.translateTo(svg.transition(d3.transition().duration(snap ? 0 : 750)), offset_x, offset_y, o.tip_anchor(svgSize.width, svgSize.height))
    // only clear this once the view has actually been anchored, so a job-triggered
    // preserveView redraw can't consume it before the first real draw
    initialDraw = false
  }
}

// bring the view back to whatever the last draw focused on (the tip, or the block
// being mined on top of it), anchored where the initial draw placed it. Keeps the
// current zoom level; just re-pans.
function recenter() {
  log_view("recenter", { to: lastTipPos, orientation: o.name })
  let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
  zoom.translateTo(svg.transition(d3.transition().duration(500)), lastTipPos.x, lastTipPos.y, o.tip_anchor(svgSize.width, svgSize.height))
}

// close every open block info box (and its connector)
function closeAllDescriptions() {
  descLayer.selectAll(".block-description").remove()
  connectorLayer.selectAll(".link-block-description").remove()
}

// radius of the ring the labels are pulled toward, around the block centre
const MINING_CLOUD_RADIUS = BLOCK_SIZE * 3

// persistent force layout for the pool-name clouds. Kept across redraws so labels keep
// their positions (and keep gently drifting) instead of teleporting each frame.
let miningSim = null
let miningLabelNodes = new Map() // key -> { key, name, bx, by, x, y }
// which labels, around which blocks, the simulation was last reheated for
let miningCloudKey = null

// pull each label toward a ring of `radius` around ITS OWN block centre (bx, by).
// d3.forceRadial only supports one shared centre, so we roll our own.
function forcePerNodeRadial(radius, strength) {
  let nodes
  function force(alpha) {
    for (const d of nodes) {
      const dx = d.x - d.bx, dy = d.y - d.by
      const r = Math.sqrt(dx * dx + dy * dy) || 1e-6
      const k = (radius - r) * strength * alpha / r
      d.vx += dx * k
      d.vy += dy * k
    }
  }
  force.initialize = n => { nodes = n }
  return force
}

// push labels away from block centres so they don't sit on top of older blocks
function forceAvoidBlocks(blocks, minDist, strength) {
  let nodes
  function force(alpha) {
    for (const d of nodes) {
      for (const b of blocks) {
        const dx = d.x - b.x, dy = d.y - b.y
        const dist = Math.sqrt(dx * dx + dy * dy) || 1e-6
        if (dist < minDist) {
          const k = (minDist - dist) * strength * alpha / dist
          d.vx += dx * k
          d.vy += dy * k
        }
      }
    }
  }
  force.initialize = n => { nodes = n }
  return force
}

// tick handler: move the labels and their connector lines to the simulation positions
function mining_cloud_tick() {
  miningLinkLayer.selectAll("line.mining-pool-link")
    .attr("x1", d => d.bx).attr("y1", d => d.by)
    .attr("x2", d => d.x).attr("y2", d => d.y)
  miningLabelLayer.selectAll("g.mining-pool")
    .attr("transform", d => "translate(" + d.x + "," + d.y + ")")
}

// throw the pool-name cloud away. The labels are held in place by a running force
// simulation in the coordinates of the orientation they were laid out in, so carrying
// them over to another one sends them flying across the viewport. Jobs arrive several
// times a second, so the cloud is rebuilt almost immediately - seeded fresh around its
// block in the new orientation.
function clear_mining_pool_clouds() {
  if (miningSim) miningSim.stop()
  miningLabelNodes = new Map()
  miningCloudKey = null
  miningLinkLayer.selectAll("line.mining-pool-link").remove()
  miningLabelLayer.selectAll("g.mining-pool").remove()
}

// lay out each being-mined block's pool names in a live force cloud around it:
// repulsion + collision keep names apart, a per-node radial pull keeps them ringed
// around their block, and a repelling force keeps them off the other blocks. Node
// objects persist across redraws so positions are continuous, and the simulation is
// gently reheated each draw so the labels keep drifting.
function draw_mining_pool_clouds(root_node, htoi) {
  let mining_nodes = root_node.descendants().filter(d => d.data.data.status == "mining")

  // obstacles: every real block centre, for the avoid force
  let obstacles = root_node.descendants()
    .filter(d => d.data.data.status != "mining")
    .map(d => ({ x: o.x(d, htoi), y: o.y(d, htoi) }))

  // resolve the desired labels into persistent node objects (keyed by block + name)
  let wanted = new Map()
  mining_nodes.forEach(node => {
    const cx = o.x(node, htoi), cy = o.y(node, htoi)
    // every pool mining on this block gets its own label, however many there are
    const labels = node.data.data.mining_pools
    const n = labels.length
    labels.forEach((name, i) => {
      const key = node.data.data.hash + "|" + name
      let d = miningLabelNodes.get(key)
      if (d === undefined) {
        // seed a new label on the ring near its block so it eases into place
        const a = (i / n) * 2 * Math.PI
        d = { key, x: cx + MINING_CLOUD_RADIUS * Math.cos(a), y: cy + MINING_CLOUD_RADIUS * Math.sin(a) }
      }
      d.name = name
      d.bx = cx
      d.by = cy
      wanted.set(key, d)
    })
  })
  miningLabelNodes = wanted
  let nodeArray = Array.from(wanted.values())

  // connector lines (in the lower layer, so they come out from behind the block) and
  // label groups, keyed so they persist across draws
  miningLinkLayer.selectAll("line.mining-pool-link")
    .data(nodeArray, d => d.key)
    .join(enter => enter.append("line").attr("class", "mining-pool-link"))

  miningLabelLayer.selectAll("g.mining-pool")
    .data(nodeArray, d => d.key)
    .join(enter => {
      let group = enter.append("g").attr("class", "mining-pool")
      group.append("rect").attr("class", "mining-pool-bg")
      group.append("text").attr("class", "mining-pool-text")
        .attr("text-anchor", "middle").attr("dy", ".35em")
      return group
    })

  // (re)set label text and size the background to fit it (names can change)
  miningLabelLayer.selectAll("g.mining-pool").each(function (d) {
    let group = d3.select(this)
    group.select("text").text(d.name)
    fit_rect_to_text(group, 3, 1)
  })

  if (nodeArray.length == 0) {
    if (miningSim) miningSim.stop()
    return
  }

  const char_w = 5.2 // rough width per character at 8px, for collision sizing
  if (miningSim === null) {
    miningSim = d3.forceSimulation().on("tick", mining_cloud_tick)
  }
  miningSim.nodes(nodeArray)
  miningSim
    .force("charge", d3.forceManyBody().strength(-20))
    .force("collide", d3.forceCollide().radius(d => (d.name.length * char_w) / 2 + 7))
    .force("radial", forcePerNodeRadial(MINING_CLOUD_RADIUS, 0.12))
    .force("avoid", forceAvoidBlocks(obstacles, BLOCK_SIZE * 3, 0.6))
  // Gentle reheat so labels drift into place rather than snapping - but only when the
  // cloud actually changed. Jobs arrive several times a second and mostly just repeat
  // what we already draw; reheating on every one of them kept the labels permanently
  // in motion instead of letting them come to rest.
  let cloud_key = nodeArray.map(d => d.key + "@" + d.bx + "," + d.by).sort().join("|")
  if (cloud_key !== miningCloudKey) {
    miningCloudKey = cloud_key
    miningSim.alpha(0.4).restart()
  }
  // place everything immediately so new labels/lines don't flash at the origin
  mining_cloud_tick()
}

// size each miner background box to fit its (already positioned) text. Text metrics
// (getBBox) only become reliable once the element is laid out, so this also runs on
// zoom to correct any boxes measured before their text was fully rendered.
function recalc_miner_boxes() {
  g.selectAll(".block-miner-group").each(function () {
    let text = d3.select(this).select("text.block-miner").node()
    let bb = text.getBBox()
    d3.select(this).select("rect.block-miner-bg")
      .attr("x", bb.width ? bb.x : 0).attr("y", bb.y)
      .attr("width", bb.width ? bb.width : 0).attr("height", bb.height)
  })
}

// measure each tip-status label and stack the boxes just off the block. Runs on draw
// and on zoom, for the same text-metric reason as recalc_miner_boxes().
function recalc_tip_boxes() {
  const bottom_edge = -1 * ((BLOCK_SIZE / 2) + 3)
  g.selectAll(".tip-info").each(function () {
    let rows = d3.select(this).selectAll("g.tip-info-row")
    let n = rows.size()
    rows.each(function (d, j) {
      let row = d3.select(this)
      let w = row.select("text").node().getComputedTextLength() + 2 * TIP_PAD_X
      let top_y = bottom_edge - TIP_BOX_H - (n - 1 - j) * (TIP_BOX_H + TIP_ROW_GAP)
      row.attr("transform", "translate(" + BLOCK_DEPTH/2 + "," + top_y + ")")
      row.select("rect").attr("x", -BLOCK_SIZE/2).attr("y", 0).attr("width", w).attr("height", TIP_BOX_H)
      row.select("text").attr("x", -BLOCK_SIZE/2 + TIP_PAD_X).attr("y", TIP_BOX_H/2)
    })
  })
}

// size each signalling chip to its text and stack the chips up from the bottom-left
// corner of the block face. Runs on draw and on zoom, for the same text-metric reason
// as recalc_miner_boxes().
function recalc_signal_chips() {
  g.selectAll("g.signal-chips").each(function () {
    let chips = d3.select(this).selectAll("g.signal-chip")
    let n = chips.size()
    chips.each(function (d, j) {
      let chip = d3.select(this)
      let w = chip.select("text").node().getComputedTextLength() + 2 * CHIP_PAD_X
      let top_y = BLOCK_SIZE/2 - CHIP_INSET - CHIP_H - (n - 1 - j) * (CHIP_H + CHIP_GAP)
      chip.attr("transform", "translate(" + (-BLOCK_SIZE/2 + CHIP_INSET) + "," + top_y + ")")
      chip.select("rect").attr("x", 0).attr("y", 0).attr("width", w).attr("height", CHIP_H)
      chip.select("text").attr("x", CHIP_PAD_X).attr("y", CHIP_H/2)
    })
  })
}

// recursivly collapses linear branches of blocks longer than x,
// starting from the root until all tips are reached.
function stripUninteresting(node, x) {
  if (node.children) {
    node.children.forEach(child => {
      let nextForkOrTip = findNextInteresting(child)
      let distance_between_nodes = nextForkOrTip.depth - child.depth
      if (distance_between_nodes > x) {
        child.children[0].children = [nextForkOrTip.parent];
      }
      stripUninteresting(nextForkOrTip, x)
    })
  }
}

function findNextInteresting(node) {
  if (isInteresting(node)) {
    return node;
  }
  for (const descendant of node) {
    if (isInteresting(descendant)) {
      return descendant;
    }
  }
  return null;
}

function isInteresting(node) {
  if (node.children === undefined) {
    // the node is a tip
    return true
  } else if (node.children.length > 1) {
    // the node is a fork
    return true
  } else if (node.data.status != "in-chain") {
    // the node has a status != "in-chain"
    return true
  }
  if (state_data.countdown) {
    if (node.data.height == state_data.countdown.height) {
      return true
    }
  }
  return false
}

function gen_treemap() {
  // nodeSize fully determines the layout (it overrides any .size()), so the tree
  // only needs a fixed node size here.
  return d3.tree().nodeSize([NODE_SIZE, NODE_SIZE]);
}

// position of the "n blocks hidden" label: the midpoint of the collapsed link,
// shifted by the orientation-specific offset
function hidden_text_x(d, htoi) {
  return o.x(d.target, htoi) - (o.x(d.target, htoi) - o.x(d.source, htoi)) / 2 + o.hidden_blocks_text.offset_x
}
function hidden_text_y(d, htoi) {
  return o.y(d.target, htoi) - (o.y(d.target, htoi) - o.y(d.source, htoi)) / 2 + o.hidden_blocks_text.offset_y
}

// Draws (or clears) the countdown marker line + label in the tree, and the large
// "N blocks until <label>" overlay. `root_node`'s cross-axis coordinate (d.x, the
// tree layout's own x) is shared by both orientations, only its final x/y slot
// differs - see the `orientations` config.
function draw_countdown(htoi, max_height, root_node) {
  const cd = state_data.countdown
  const display = document.getElementById("countdown-display")

  if (!cd) {
    countdownLayer.selectAll("*").remove()
    if (display) display.style.display = "none"
    return
  }

  const blocks_left = cd.height - max_height

  if (display) {
    if (blocks_left > 0) {
      const unit = blocks_left == 1 ? "block" : "blocks"
      display.textContent = `${blocks_left} ${unit} until ${cd.label}`
      display.style.display = ""
    } else if (blocks_left == 0) {
      display.textContent = `${cd.label} now`
      display.style.display = ""
    } else if (blocks_left < 0 && blocks_left > -10) {
      const unit = blocks_left == -1 ? "block" : "blocks"
      display.textContent = `${blocks_left * -1} ${unit} ago: ${cd.label}`
      display.style.display = ""
    } else {
      display.style.display = "none"
    }
  }

  // Only draw the marker once we're close to the target; once shown (including
  // after the target height has been reached) it is never hidden again, since
  // blocks_left stays <= 0 (and thus < the threshold) from then on.
  if (blocks_left >= COUNTDOWN_MARKER_THRESHOLD) {
    countdownLayer.selectAll("*").remove()
    return
  }

  const idx = cd.height in htoi ? htoi[cd.height] : htoi[max_height] + blocks_left
  // Sit the marker between height-1 and height rather than centered on the
  // target block itself (assumes the two are adjacent slots, which holds here
  // since the backend always keeps height-2..height+2 once mined).
  const along = o.countdown_along(idx - 0.4)

  const cross_values = root_node.descendants().map(d => d.x)
  const cross_min = Math.min(...cross_values) - NODE_SIZE
  const cross_max = Math.max(...cross_values) + NODE_SIZE
  const label_cross = cross_min - 16

  const line_coords = o.countdown_line(along, cross_min, cross_max)
  const label_pos = o.countdown_label_pos(along, label_cross)

  countdownLayer.selectAll("line.countdown-line")
    .data([line_coords])
    .join("line")
    .attr("class", "countdown-line")
    .attr("x1", d => d.x1)
    .attr("y1", d => d.y1)
    .attr("x2", d => d.x2)
    .attr("y2", d => d.y2)

  countdownLayer.selectAll("text.countdown-label")
    .data([label_pos])
    .join("text")
    .attr("class", "countdown-label")
    .style("text-anchor", "middle")
    .attr("x", d => d.x)
    .attr("y", d => d.y)
    .attr("transform", d => `rotate(${o.block_text_rotate}, ${d.x}, ${d.y})`)
    .attr("dy", ".3em")
    .text(cd.label)
}

function onBlockClick(c, d) {
  // the block group, whatever depth inside it was clicked. It is the group that
  // carries the block's absolute x/y attrs, which the description is anchored on,
  // so walking up to it is not optional - a nearer ancestor has no coordinates.
  let blockGroup = d3.select(c.target.closest("g.block"))
  // only react to clicks on an actual block group
  if (blockGroup.empty()) return
  // blocks that only come from the stratum feed have no header data for the info card
  if (from_stratum_feed(d.data.data)) return

  // toggle: if this block already has an open description, close it
  let existing = descLayer.selectAll(".block-description")
    .filter(function () { return this.getAttribute("data-hash") == d.data.data.hash })
  if (!existing.empty()) {
    existing.remove()
    connectorLayer.selectAll(".link-block-description")
      .filter(function () { return this.getAttribute("data-hash") == d.data.data.hash })
      .remove()
    return
  }

  // the description lives in the overlay layer (drawn on top of everything) and is
  // anchored at the block's absolute position, which the block stores as x/y attrs
  const block_x = +blockGroup.attr("x")
  const block_y = +blockGroup.attr("y")
  // offset of the info box from the block, in the block's local coordinate space
  const description_offset = { x: 50, y: -25 }
  let pos = { x: description_offset.x, y: description_offset.y }

  let descGroup = descLayer.append("g")
    .attr("class", "block-description")
    .attr("data-hash", d.data.data.hash)
    .attr("transform", "translate(" + block_x + "," + block_y + ")")

  // connector link from the block to the centre of the info box. It lives in its own
  // layer below the blocks (both groups are anchored at the block, so its origin
  // [0, 0] is the block centre) and carries the same data-hash to stay in sync.
  let connector = connectorLayer.append("path")
    .attr("class", "link link-block-description")
    .attr("data-hash", d.data.data.hash)
    .attr("transform", "translate(" + block_x + "," + block_y + ")")

  let cardHolder = descGroup.append("g")
    .attr("transform", "translate(" + pos.x + "," + pos.y + ")")
    .call(
      d3.drag()
        .on("start", dragstarted)
        .on("drag", dragged)
        .on("end", dragended)
    )

  function connectorPath() {
    return d3.linkHorizontal()({
      source: [0, 0],
      target: [
        pos.x + (card.node().getBoundingClientRect().width  / d3.zoomTransform(svg.node()).k) / 2,
        pos.y + (card.node().getBoundingClientRect().height / d3.zoomTransform(svg.node()).k) / 2
      ]
    })
  }

  function dragstarted() { d3.select(this).raise().attr("cursor", "grabbing"); }
  function dragged(event) {
    pos.x += event.dx;
    pos.y += event.dy;
    cardHolder.attr("transform", "translate(" + pos.x + "," + pos.y + ")");
    connector.attr("d", connectorPath());
  }
  function dragended() { d3.select(this).attr("cursor", "grab"); }

  function closeDescription() {
    descGroup.remove()
    connector.remove()
  }

  let status_text = "";
  // block description: tip status for nodes
  if (d.data.data.status != "in-chain") {
    d.data.data.status.slice().reverse().forEach(status => {
      status_text += `<span class="text-monospace tip-status-color-fill-${status.status}">▆ </span>`
      status_text += `<span>${status.count}x ${status.status}: ${status.nodes.map(n => n.name).join(", ")}`
    })
  }

  // version-bit deployments this block signals for, as a comma separated list
  let signalling = signalled_deployments(d.data.data).map(dep => dep.name).join(", ")

  // The full block can only be fetched (via the API) for stale blocks: those
  // that are a tip on some node but not the active chain. The active tip and
  // to-be-mined blocks are not served, so we don't offer a link for them.
  let is_stale = Array.isArray(d.data.data.status)
    && !d.data.data.status.some(s => s.status == "active");
  // The `download` attribute renames the fetched file to `<height>-<hash>.<ext>`.
  let block_basename = `${d.data.data.height}-${d.data.data.hash}`;
  let block_url = `api/${state_selected_network_id}/block/${d.data.data.hash}`;
  let download = is_stale
    ? `<a href="${block_url}/hex" download="${block_basename}.hex">hex</a> `
      + `<a href="${block_url}/bin" download="${block_basename}.bin">binary</a></span></div>`
    : "";

  let cardWrapper = cardHolder.append("foreignObject")
    .attr("height", "20")
    .attr("width", "600")
  let card = cardWrapper
    .append("xhtml:div")
      .attr("class", "card m-0 p-0 block-info-card")
  let headerDiv = card.append("xhtml:div").attr("class", "card-header")
  headerDiv.append()
    .html(`<span>Header at height <span class="copyable" title="click to copy" onClick='copyToClipboard("${d.data.data.height}", "height")'>${d.data.data.height}</span></span>`)
  headerDiv.append()
    .style("float", "right")
    .html(`<button class="btn btn-close"></button>`)
    .on("click", closeDescription);

  card.append("div")
    .attr("class", "card-body")
    .html(`
          <div class="container">
            <div class="row small">
              <div class="col small">
                <div class="row copyable" title="click to copy" onClick='copyToClipboard("${d.data.data.hash}", "hash")'><span class="col-2">hash</span><span class="col-10 font-monospace small">${d.data.data.hash}</span></div>
                <div class="row copyable" title="click to copy" onClick='copyToClipboard("${d.data.data.prev_blockhash}", "previous hash")'><span class="col-2">previous</span><span class="col-10 font-monospace small">${d.data.data.prev_blockhash}</span></div>
                <div class="row copyable" title="click to copy" onClick='copyToClipboard("${d.data.data.merkle_root}", "merkle root")'><span class="col-2">merkleroot</span><span class="col-10 font-monospace small">${d.data.data.merkle_root}</span></div>
                <div class="row">
                  <span class="col-2">timestamp</span><span class="col-4">${d.data.data.time}</span>
                  <span class="col-2">version</span><span class="col-4 font-monospace">0x${d.data.data.version.toString(16)}</span>
                  ${ signalling != "" ? '<span class="col-2">signalling</span><span class="col-4">' + signalling + '</span>' : '' }
                  <span class="col-2">nonce</span><span class="col-4 font-monospace">0x${d.data.data.nonce.toString(16)}</span>
                  <span class="col-2">bits</span><span class="col-4 font-monospace">0x${d.data.data.bits.toString(16)}</span>
                  <span class="col-2">difficulty</span><span class="col-4 font-monospace">${d.data.data.difficulty_int}</span>
                  ${ d.data.data.miner != "" ? '<span class="col-2">miner</span><span class="col-4 font-monospace">' + d.data.data.miner + '</span>' : '' }
                  ${ is_stale ? '<span class="col-2">download</span><span class="col-4">' + download + '</span>' : "" }
                </div>
                <div class="row"><span class="col">${status_text}</span></div>
              </div>
            </div>
          </div>
      `)
  cardWrapper.attr("height", card.node().getBoundingClientRect().height / d3.zoomTransform(svg.node()).k )
  cardWrapper.attr("width", card.node().getBoundingClientRect().width / d3.zoomTransform(svg.node()).k )

  // draw the connector now that the card size is known
  connector.attr("d", connectorPath())
}

function get_active_height_or_0(node) {
  let active_tips = node.tips.filter(tip => tip.status == "active")
  if (active_tips.length > 0) {
    return active_tips[0].height
  }
  return 0
}

function get_active_hash_or_fake(node) {
  let active_tips = node.tips.filter(tip => tip.status == "active")
  if (active_tips.length > 0) {
    return active_tips[0].hash
  }
  return "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffdead"
}

// Nodes we didn't fetch ourselves but imported from another fork-observer
// instance carry that instance's name. Mark them with a muted "via <instance>"
// so it's clear the data is second-hand.
function via_label(node) {
  if (!node.remote_source) return ""
  return `<span class="nt-via" title="imported from the fork-observer instance '${node.remote_source}'">via ${node.remote_source}</span>`
}

function ago(timestamp) {
  const rtf = new Intl.RelativeTimeFormat("en", {
    style: "narrow",
    numeric: "always",
  });
  const now = new Date()
  const utc_seconds = (now.getTime() + now.getTimezoneOffset()*60) / 1000;
  const seconds = parseInt(timestamp - utc_seconds);
  if (seconds > -90) {
    return rtf.format(seconds, "seconds");
  }
  const minutes = parseInt(seconds/60);
  if (minutes > -60) {
    return rtf.format(minutes, "minutes");
  }
  const hours = parseInt(minutes/60);
  if (hours > -24) {
    return rtf.format(hours, "hours");
  }
  const days = parseInt(hours/60);
  if (days > -30) {
    return rtf.format(days, "days");
  }
  const months = parseInt(days/31);
  if (months > -12) {
    return rtf.format(months, "months");
  }

  return "a long time ago"
}

async function draw_nodes() {
  nodeInfoRow.html(null);

  let table = nodeInfoRow.append("table").attr("class", "node-table")
  table.append("thead").append("tr").html(`
    <th>node</th>
    <th>implementation</th>
    <th>tip changed</th>
    <th class="nt-num">height</th>
    <th>tip hash</th>
  `)

  table.append("tbody")
    .selectAll("tr")
    .data(state_data.nodes.sort((a, b) => get_active_height_or_0(a) - get_active_height_or_0(b)))
    .enter()
    .append("tr")
      .attr("class", "node-row")
      // expose the height/tip-hash hues as CSS vars so the height and tip-hash
      // cells can be tinted: same height -> same height-chip color, same tip ->
      // same hash-chip color.
      .attr("style", d => {
        const height_hue = parseInt(get_active_height_or_0(d) * 90, 10) % 360
        const hash_hue = (parseInt(get_active_hash_or_fake(d).substring(58), 16) + 120) % 360
        return `--height-hue: ${height_hue}; --hash-hue: ${hash_hue};`
      })
      .html(d => {
        const height = get_active_height_or_0(d)
        const hash = get_active_hash_or_fake(d)
        const version = d.version == "unknown" ? `${d.implementation} (version unknown)` : d.version
        return `
        <td class="nt-name">
          <span class="node-status-dot ${d.reachable ? "is-up" : "is-down"}" title="${d.reachable ? "reachable" : "unreachable"}"></span>
          <span class="nt-name-text" title="${d.name}">${d.name}</span>
          ${via_label(d)}
          ${d.reachable ? "" : "<span class='badge text-bg-danger'>unreachable</span>"}
          ${d.description ? `<div class="nt-desc" onclick="this.classList.toggle('nt-desc-open')">${d.description}</div>` : ""}
        </td>
        <td class="nt-impl">${version}</td>
        <td class="text-muted-soft nt-time"><span class="relativeTimestamp" data-timestamp=${d.last_changed_timestamp}>${ago(d.last_changed_timestamp)}</span></td>
        <td class="nt-num"><span class="height-chip">${height}</span></td>
        <td><code class="hash-chip" title="click to copy full tip hash" onclick="copyToClipboard('${hash}', 'tip hash')">…${hash.substring(44, 64)}</code></td>
      `})
}

orientationSelect.on("input", async function() {
  o = orientations[this.value]
  await draw({ reason: "orientation changed", snap: true, clearPoolCloud: true })
})

// Set the orientation by checking the screen width and height
{
  const supported_orientations = [
    { name: "left to right", value: "left-to-right" },
    { name: "bottom to top", value: "bottom-to-top" }
  ]

  let browser_size_ratio = (window.innerWidth || document.documentElement.clientWidth || document.body.clientWidth) / (window.innerHeight|| document.documentElement.clientHeight|| document.body.clientHeight);

  var choosen_orientation = "left-to-right"
  if (browser_size_ratio < 1) {
    choosen_orientation = "bottom-to-top"
  }

  orientationSelect.selectAll('option')
	  .data(supported_orientations)
	  .enter()
	    .append('option')
	    .attr('value', d => d.value)
	    .text(d => d.name)
	    .property("selected", d => d.value == choosen_orientation)
}
o = orientations[orientationSelect.node().value]
