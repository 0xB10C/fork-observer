const NODE_SIZE = 100
const MAX_USIZE = 18446744073709551615;
const BLOCK_SIZE = 50
const MIN_DIFFICULTY = 1

const orientationSelect = d3.select("#orientation")

const orientations = {
  "bottom-to-top": {
    x: (d, _) => d.x,
    y: (d, htoi) => -htoi[d.data.data.height] * NODE_SIZE,
    linkDir: (htoi) => d3.linkVertical().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
    hidden_blocks_text: {offset_x: -15, offset_y: 0, anchor: "left"},
    block_text_rotate: -90,
  },
  "left-to-right": {
    x: (d, htoi) => htoi[d.data.data.height] * NODE_SIZE,
    y: (d, _) => d.x,
    linkDir: (htoi) => d3.linkHorizontal().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
    hidden_blocks_text: {offset_x: 0, offset_y: 15, anchor: "middle"},
    block_text_rotate: 0,
  },
};

const status_to_color = {
  "active": "lime",
  "invalid": "fuchsia",
  "valid-fork": "cyan",
  "valid-headers": "red",
  "headers-only": "yellow",
}

// node status indicator
const indicator_radius = 8
const indicator_margin = 1

let o = orientations["left-to-right"];

let svg = d3
    .select("#drawing-area")
    .style("border", "1px solid")

let initialDraw = true

// enables zoom and panning
const zoom = d3.zoom().scaleExtent([0.15, 2]).on( "zoom", e => g.attr("transform", e.transform) )
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

  let links = g
    .selectAll(".link-block-block")
    .data(root_node.links(), d => `${d.source.data.data.hash}-${d.target.data.data.hash}`)
    .join(
      enter => {
        enter.append("path")
          .attr("class", "link link-block-block")
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
          .attr("rx", 5)
          .attr("fill", "white")
          .attr("stroke", d => d.data.data.difficulty_int == MIN_DIFFICULTY ? "darksalmon" : "lightgray")
          .attr("stroke-width", d => d.data.data.difficulty_int == MIN_DIFFICULTY ? 3 : 1)
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

        let pool_text = block_child_group
          .insert("text")
          .classed("block-pool-name", true)
          .attr("transform", `rotate(${o.block_text_rotate},0,0)`)
          .attr("dy", "4em")
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

          pool_text
            .filter(d => d.data.data.height == max_height)
            .style("opacity", 0)
            .transition(d3.transition().duration(600))
            .style("opacity", 1)

          height_text
            .filter(d => d.data.data.height == max_height)
            .style("font-size", "0px")
            .transition(d3.transition().duration(600))
            .style("font-size", "10px")
        }

        return newBlocks
      },
      update => {
        update
          .transition(d3.transition().duration(600))
          .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")
        update.selectAll(".block-pool-name")
          .attr("transform", `rotate(${o.block_text_rotate},0,0)`)

        update.raise()
        return update
      }
    );

  let node_groups = g
    .selectAll(".node-tip-status-indicator")
    .data(root_node.descendants().filter(d => d.data.data.status != "in-chain" && d.data.data.status != "mining"))
    .join("g")
    .classed("node-tip-status-indicator", true)
    .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")

  // build the rect + text once per indicator on enter, otherwise every redraw
  // would append another copy to the existing indicator groups
  let indicators = node_groups.selectAll("g.tip-status-indicator")
    .data(d => d.data.data.status)
    .join(enter => {
      let group = enter.append("g").attr("class", "tip-status-indicator")
      group.append("rect")
        .attr("width", indicator_radius*2)
        .attr("height", indicator_radius*2)
        .attr("rx", 1)
        .attr("r", indicator_radius)
        .attr("y", -BLOCK_SIZE/2 - indicator_radius)
      group.append("text")
        .attr("y", -BLOCK_SIZE/2)
        .attr("dy", ".35em")
      return group
    })

  // refresh the parts that depend on the (possibly changed) status data
  indicators.select("rect")
    .attr("x", (d, i) => (BLOCK_SIZE/2) - i * (indicator_radius + indicator_margin) * 2 - indicator_radius)
    .attr("class", d => "tip-status-color-fill-" + d.status)

  indicators.select("text")
    .attr("dx", (d, i) => (BLOCK_SIZE/2) - i * (indicator_radius + indicator_margin) * 2)
    .text(d => d.count)

  let offset_x = 0;
  let offset_y = 0;
  let max_height_tip = root_node.leaves().filter(d => d.data.data.height == max_height)[0]
  if (max_height_tip !== undefined) {
    offset_x = o.x(max_height_tip, htoi);
    offset_y = o.y(max_height_tip, htoi);
  }

  // raise the blocks to make sure they are drawn over the links, then the tip
  // status markers over the blocks
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

  zoom.scaleBy(svg, 1);
  let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
  zoom.translateTo(svg.transition(d3.transition().duration(initialDraw ? 0 : 750)), offset_x, offset_y, [(svgSize.width)/2, (svgSize.height)/2])

  svg.select("#legend").attr("x", svg.node().clientWidth - 150 - 10)

  initialDraw = false
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
      .attr("class", "card m-0 p-0 border")
  let headerDiv = card.append("xhtml:div").attr("class", "card-header border")
  headerDiv.append()
    .html(`<span>Header at height <span style="cursor: pointer" onClick='window.prompt("height:", "${d.data.data.height}")'>${d.data.data.height}</span></span>`)
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
                <div class="row" style="cursor: pointer" onClick='window.prompt("hash:", "${d.data.data.hash}")'><span class="col-2">hash</span><span class="col-10 font-monospace small">${d.data.data.hash}</span></div>
                <div class="row" style="cursor: pointer" onClick='window.prompt("previous hash:", "${d.data.data.prev_blockhash}")'><span class="col-2">previous</span><span class="col-10 font-monospace small">${d.data.data.prev_blockhash}</span></div>
                <div class="row" style="cursor: pointer" onClick='window.prompt("merkle root:", "${d.data.data.merkle_root}")'><span class="col-2">merkleroot</span><span class="col-10 font-monospace small">${d.data.data.merkle_root}</span></div>
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

function node_description(description) {
  return `
    <p class="mb-0 text-truncate" onclick="this.style.whiteSpace = 'normal'">
      <span>${description}</span>
    </p>
  `
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
  nodeInfoRow.selectAll('.node-info')
    .data(state_data.nodes.sort((a, b) => get_active_height_or_0(a) - get_active_height_or_0(b)))
    .enter()
    .append("div")
      .attr("class", "row-cols-auto px-1")
      .html(d => `
      <div class="col border rounded node-info my-2" style="min-height: 8rem; width: 16rem;">
        <h5 class="card-title py-0 mt-1 mb-0">
          <span class="mx-2 mt-1 d-inline-block text-truncate" style="max-width: 15rem;">
            <img class="invert" src="static/img/node.svg" height=28 alt="Node symbol">
            ${d.name}
          </span>
        </h5>
        <div class="px-2 small">
          ${d.reachable ? "": "<span class='badge text-bg-danger'>unreachable</span>"}
          <span class='badge text-bg-secondary small'>${d.implementation} ${d.version.replaceAll("/", "").replaceAll("Satoshi:", "").replace("unknown", "(version unknown)")}</span>
        </div>

        <div class="px-2">
          ${node_description(d.description)}
        </div>
        <div class="px-2">
          <span class="small">tip changed <span class="relativeTimestamp" data-timestamp=${d.last_changed_timestamp}>${ago(d.last_changed_timestamp)}</span>
        </div>
        <div class="px-2" style="background-color: hsl(${parseInt(get_active_height_or_0(d) * 90, 10) % 360}, 50%, 75%)">
          <span class="small text-color-dark"> height: ${get_active_height_or_0(d)}
        </div>
        <div class="px-2 rounded-bottom" style="background-color: hsl(${(parseInt(get_active_hash_or_fake(d).substring(58), 16) + 120) % 360}, 50%, 75%)">
          <details>
            <summary style="list-style: none;">
              <span class="small text-color-dark">
                tip hash: …${get_active_hash_or_fake(d).substring(54, 64)}
              </span>
            </summary>
            <span class="small text-color-dark">
              ${get_active_hash_or_fake(d)}
            </span>
          </details>
        </div>
      </div>
    `)
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
