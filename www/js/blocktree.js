const NODE_SIZE = 120
const MAX_USIZE = 18446744073709551615;
const BLOCK_SIZE = 50
const BLOCK_DEPTH = 9 // depth of the 3D extrusion (offset toward the top-right)
const MIN_DIFFICULTY = 1

const orientationSelect = d3.select("#orientation")

const orientations = {
  "bottom-to-top": {
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
  },
  "left-to-right": {
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
  },
};

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

let o = orientations["left-to-right"];

// absolute position of the current tip, remembered so the "recenter" button can
// bring the view back to it after the user pans/zooms away
let lastTipPos = { x: 0, y: 0 }

let svg = d3
    .select("#drawing-area")

let initialDraw = true

// enables zoom and panning
const zoom = d3.zoom().scaleExtent([0.15, 5]).on( "zoom", e => {
  g.attr("transform", e.transform)
  // re-measure the text-fitted boxes; their metrics can be stale if they were first
  // sized before the text was fully laid out
  recalc_miner_boxes()
  recalc_tip_boxes()
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

// overlay layer that always holds the open block descriptions (info boxes). It is
// raised to the top on every draw so the boxes are never painted over by blocks or
// tip status markers.
let descLayer = g
    .append("g")
    .attr("id", "descriptions")

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

  var treeData = d3
    .stratify()
    .id(d => d.id)
    .parentId(function (d) {
      // d3js requires the first prev block hash to be null
      return (d.prev_id == MAX_USIZE ? null : d.prev_id)
    })(header_infos);

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

  // assigns the data to a hierarchy using parent-child relationships
  // and maps the node data to the tree layout. Make sure the headers
  // and forks are sorted deterministically. This means, they don't
  // change on redraws, which is nicer.
  const root_node = treemap(
    d3.hierarchy(treeData).sort((a, b) =>
      d3.ascending(a.data.data.height, b.data.data.height) ||
      d3.ascending(a.data.data.hash, b.data.data.hash))
  )
  const max_height = Math.max(...header_infos.map(d => d.height))

  return [root_node, max_height, htoi]
}

function draw() {
  let data = state_data

  // nothing to draw if there are no headers
  if (data.header_infos.length == 0) {
    return
  }

  const [root_node, max_height, htoi] = preprocess_data(data)

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
        update.transition(d3.transition().duration(600))
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
          .classed("being-mined", d => d.target.data.data.status == "mining")
          .transition(d3.transition().duration(300))
          .attr("stroke-dashoffset", 0)
          .attr("stroke-opacity", 0.2)
          .transition(d3.transition().duration(300))
          .attr("stroke-opacity", 1)
      },
      update => {
        update
          .transition(d3.transition().duration(600))
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
        update
          .transition(d3.transition().duration(600))
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
          .attr("stroke-opacity", d => d.data.data.status == "mining" ? 0.2 : 1)
          .classed("being-mined", d => d.data.data.status == "mining")

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

          height_text
            .filter(d => d.data.data.height == max_height)
            .style("font-size", "0px")
            .transition(d3.transition().duration(600))
            .style("font-size", "11px")
        }

        return newBlocks
      },
      update => {
        update
          .transition(d3.transition().duration(600))
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

  // size the miner background box to fit its (already positioned) text
  recalc_miner_boxes()

  // tip info label: a stack of colored "Nx status" boxes next to each tip block. the
  // whole group is rotated like the miner text so it sits on the opposite side.
  let node_groups = g
    .selectAll(".tip-info")
    .data(root_node.descendants().filter(d => d.data.data.status != "in-chain" && d.data.data.status != "mining"),
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
  let max_height_tip = root_node.leaves().filter(d => d.data.data.height == max_height)[0]
  if (max_height_tip !== undefined) {
    offset_x = o.x(max_height_tip, htoi);
    offset_y = o.y(max_height_tip, htoi);
  }

  // stack, bottom to top: 3D depth faces, then the links over them, then the block
  // front faces (so links tuck behind the front face and look centered), then the
  // tip status markers
  backFaces.raise()
  g.selectAll(".link-block-block").raise()
  g.selectAll(".text-blocks-not-shown").raise()
  blocks.raise()
  node_groups.raise()

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

  zoom.scaleBy(svg, 1);
  let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
  zoom.translateTo(svg.transition(d3.transition().duration(initialDraw ? 0 : 750)), offset_x, offset_y, o.tip_anchor(svgSize.width, svgSize.height))

  initialDraw = false
}

// bring the view back to the tip, anchored where the initial draw placed it. Keeps
// the current zoom level; just re-pans.
function recenter() {
  let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
  zoom.translateTo(svg.transition(d3.transition().duration(500)), lastTipPos.x, lastTipPos.y, o.tip_anchor(svgSize.width, svgSize.height))
}

// close every open block info box (and its connector)
function closeAllDescriptions() {
  descLayer.selectAll(".block-description").remove()
  connectorLayer.selectAll(".link-block-description").remove()
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

function onBlockClick(c, d) {
  let blockGroup = d3.select(c.target.parentElement.parentElement)
  // only react to clicks on an actual block group
  if (blockGroup.attr("class") == null || !blockGroup.attr("class").startsWith("block")) return

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
                  <span class="col-2">nonce</span><span class="col-4 font-monospace">0x${d.data.data.nonce.toString(16)}</span>
                  <span class="col-2">bits</span><span class="col-4 font-monospace">0x${d.data.data.bits.toString(16)}</span>
                  <span class="col-2">difficulty</span><span class="col-4 font-monospace">${d.data.data.difficulty_int}</span>
                  ${ d.data.data.miner != "" ? '<span class="col-2">miner</span><span class="col-4 font-monospace">' + d.data.data.miner + '</span>' : '' }
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
        const version = d.version.replaceAll("/", "").replaceAll("Satoshi:", "").replace("unknown", "(version unknown)")
        return `
        <td class="nt-name">
          <span class="node-status-dot ${d.reachable ? "is-up" : "is-down"}" title="${d.reachable ? "reachable" : "unreachable"}"></span>
          <span class="nt-name-text" title="${d.name}">${d.name}</span>
          ${d.reachable ? "" : "<span class='badge text-bg-danger'>unreachable</span>"}
          ${d.description ? `<div class="nt-desc" onclick="this.classList.toggle('nt-desc-open')">${d.description}</div>` : ""}
        </td>
        <td class="nt-impl">${d.implementation} ${version}</td>
        <td class="text-muted-soft nt-time"><span class="relativeTimestamp" data-timestamp=${d.last_changed_timestamp}>${ago(d.last_changed_timestamp)}</span></td>
        <td class="nt-num"><span class="height-chip">${height}</span></td>
        <td><code class="hash-chip" title="click to copy full tip hash" onclick="copyToClipboard('${hash}', 'tip hash')">…${hash.substring(44, 64)}</code></td>
      `})
}

orientationSelect.on("input", async function() {
  o = orientations[this.value]
  await draw()
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
