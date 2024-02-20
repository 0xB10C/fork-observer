const NODE_SIZE = 100
const MAX_USIZE = 18446744073709551615;

const orientationSelect = d3.select("#orientation")

const orientations = {
  "bottom-to-top": {
    x: (d, _) => d.x,
    y: (d, htoi) => -htoi[d.data.data.height] * NODE_SIZE,
    linkDir: (htoi) => d3.linkVertical().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
    hidden_blocks_text: {offset_x: -45, offset_y: 0, anchor: "left"},
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

let o = orientations["left-to-right"];

const status_to_color = {
  "active": "lime",
  "invalid": "fuchsia",
  "valid-fork": "cyan",
  "valid-headers": "red",
  "headers-only": "yellow",
}

function draw() {
  data = state_data
  let header_infos = data.header_infos;
  let tip_infos = [];
  let node_infos = data.nodes;

  hash_to_tipstatus = {}
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

  let treemap = gen_treemap(o, tip_infos.length, unique_heights);

  // assigns the data to a hierarchy using parent-child relationships
  // and maps the node data to the tree layout
  var root_node = treemap(d3.hierarchy(treeData));

  var svg = d3
    .select("#drawing-area")
    .style("border", "1px solid")

  svg.selectAll("*").remove()

  // enables zoom and panning
  const zoom = d3.zoom().scaleExtent([0.15, 2]).on( "zoom", e => g.attr("transform", e.transform) )
  svg.call(zoom)

  var g = svg
    .append("g")

  // links between the nodes
  var links = g
    .selectAll(".link-block-block")
    .data(root_node.links())
    .enter()

  // <path> between blocks
  links.append("path")
    .attr("class", "link link-block-block")
    .attr("d", o.linkDir(htoi))
    .attr("stroke-dasharray", d => d.target.data.data.height - d.source.data.data.height == 1 ? "0" : "4 5")

  // text for the not-shown blocks
  var link_texts_hidden_blocks = links
    .filter(d => d.target.data.data.height - d.source.data.data.height != 1)
    .append("text")
    .attr("class", "text-blocks-not-shown")
    .style("text-anchor", o.hidden_blocks_text.anchor)
    .style("font-size", "12px")
    .attr("x", d => o.x(d.target, htoi) - ((o.x(d.target, htoi) - o.x(d.source, htoi))/2) + o.hidden_blocks_text.offset_x )
    .attr("y", d => o.y(d.target, htoi) - ((o.y(d.target, htoi) - o.y(d.source, htoi))/2) + o.hidden_blocks_text.offset_y )
  link_texts_hidden_blocks.append("tspan")
    .text(d => (d.target.data.data.height - d.source.data.data.height -1) + " blocks")
    .attr("dy", ".3em")
  link_texts_hidden_blocks.append("tspan")
    .text("hidden")
    .attr("x", d => o.x(d.target, htoi) - ((o.x(d.target, htoi) - o.x(d.source, htoi))/2) + o.hidden_blocks_text.offset_x )
    .attr("dy", "1em")

  // adds each block as a group
  var blocks = g
    .selectAll(".block-group")
    .data(root_node.descendants())
    .enter()
    .append("g")
    .attr("class", d => "block" + (d.children ? " block--internal" : " block--leaf"))
    .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")")
    .on("click", (c, d) => onBlockClick(c, d))

  function onBlockClick(c, d) {
    let parentElement = d3.select(c.target.parentElement)

    // The on-click listener of the block propagates to the appened description elements.
      // To prevent adding a second description element of the block we return early if the
      // parentElement is not the block.
      if (parentElement.attr("class") == null || !parentElement.attr("class").startsWith("block block--")) return

      if (parentElement.selectAll(".block-description").size() > 0) {
        parentElement.selectAll(".block-description").remove()
        parentElement.selectAll(".link-block-description").attr("d", "")
      } else {

        const description_offset = { x: 50, y: -50 }
        const description_margin = { x: 0, y: 15 }
        let descGroup = parentElement.append("g")
          .attr("class", "block-description")
          .attr("transform", "translate(" + description_offset.x + "," + description_offset.y / 2 + ")")
          .each(d => { d.x = description_offset.x; d.y = description_offset.y })
          .call(
            d3.drag()
              .on("start", dragstarted)
              .on("drag", dragged)
              .on("end", dragended)
          )

        parentElement.raise()

        function dragstarted() {d3.select(this).raise().attr("cursor", "grabbing");}
        function dragged(event, d) {
          d.x += event.dx;
          d.y += event.dy;
          var link = d3.linkHorizontal()({
            source: [ 0, 0 ],
            target: [
              d.x + (card.node().getBoundingClientRect().width  / d3.zoomTransform(svg.node()).k) / 2,
              d.y + (card.node().getBoundingClientRect().height / d3.zoomTransform(svg.node()).k) / 2
            ]
          });
          parentElement.selectAll(".link-block-description").attr('d', link)
          d3.select(this).attr("transform", "translate(" + d.x + "," + d.y + ")");
        }
        function dragended() { d3.select(this).attr("cursor", "drag"); }

        let descCloseGroup = descGroup.append("g")

        let status_text = "";
        // block description: tip status for nodes
        if (d.data.data.status != "in-chain") {
          d.data.data.status.reverse().forEach(status => {
            status_text += `<span class="text-monospace tip-status-color-fill-${status.status}">▆ </span>`
            status_text += `<span>${status.count}x ${status.status}: ${status.nodes.map(n => n.name).join(", ")}`
          })
        }


        function onBlockDescriptionCloseClick(c, d) {
          let parentElement = d3.select(c.target.parentElement.parentElement.parentElement.parentElement.parentElement.parentElement)
          parentElement.selectAll(".block-description").remove()
          parentElement.selectAll(".link-block-description").attr("d", "")
        }

        let cardWrapper = descGroup.append("foreignObject")
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
          .on("click", (c, d) => onBlockDescriptionCloseClick(c, d));
      
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
                      ${ d.data.data.miner != "" ? '<span class="col-2">miner</span><span class="col-4 font-monospace">' + d.data.data.miner + '</span>' : '' }
                    </div>
                    <div class="row"><span class="col">${status_text}</span></div>
                  </div>
                </div>
              </div>
          `)
        cardWrapper.attr("height", card.node().getBoundingClientRect().height / d3.zoomTransform(svg.node()).k )
        cardWrapper.attr("width", card.node().getBoundingClientRect().width / d3.zoomTransform(svg.node()).k )
      }
    }

  blocks
    .append('path')
    .attr("class", "link link-block-description") // when modifying, check if there is a depedency on this class name.

  // rect for each block
  const block_size = 50
  blocks
    .append("rect")
    .attr("height", block_size)
    .attr("width", block_size)
    .attr("rx", 5)
    .attr("fill", "white")
    .attr("stroke", "black")
    .attr("stroke-width", "1")
    .attr("transform", "translate("+ (-block_size)/2  +", " + (-block_size)/2 + ")")

  // height in the block
  blocks
    .append("text")
    .attr("dy", ".35em")
    .attr("class", "block-text")
    .text(d => d.data.data.height);

  // miner next to the block
  blocks
    .append("text")
    .attr("transform", `rotate(${o.block_text_rotate},0,0)`)
    .attr("dy", "4em")
    .attr("class", "block-miner")
    .text(d => d.data.data.miner.length > 14 ? d.data.data.miner.substring(0, 14) + "…" : d.data.data.miner);

  var node_groups = blocks
    .filter(d => d.data.data.status != "in-chain")
    .append("g")
    .selectAll("g")
    .data(d => d.data.data.status)
    .join("g")
    .attr("class", d => "node-indicator")

  // node status indicator
  const indicator_radius = 8
  const indicator_margin = 1
  node_groups.append("rect")
    .attr("width", indicator_radius*2)
    .attr("height", indicator_radius*2)
    .attr("rx", 1)
    .attr("r", indicator_radius)
    .attr("y", -block_size/2 - indicator_radius)
    .attr("x", (d, i) => (block_size/2) - i * (indicator_radius + indicator_margin) * 2 - indicator_radius)
    .attr("class", d => "tip-status-color-fill-" + d.status)

  node_groups.append("text")
    .attr("y", -block_size/2)
    .attr("dx", (d, i) => (block_size/2) - i * (indicator_radius + indicator_margin) * 2)
    .attr("dy", ".35em")
    .attr("class", "node-indicator")
    .text(d => d.count)

  let offset_x = 0;
  let offset_y = 0;
  let max_height = Math.max(...header_infos.map(d => d.height))
  let max_height_tip = root_node.leaves().filter(d => d.data.data.height == max_height)[0]
  if (max_height_tip !== undefined) {
    offset_x = o.x(max_height_tip, htoi);
    offset_y = o.y(max_height_tip, htoi);
  }

  zoom.scaleBy(svg, 1);
  let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
  zoom.translateTo(svg, offset_x, offset_y, [(svgSize.width)/2, (svgSize.height)/2])
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

function gen_treemap(o, tips, unique_heights) {
  return d3.tree().size([tips, unique_heights]).nodeSize([NODE_SIZE, NODE_SIZE]);
}


// If nessecary, return a <details> <summary> of the description
function node_description_summary(description) {
  if (description.length > 20) {
    return `
      <details>
        <summary>
          <span>description</span>
        </summary>
        <span>${description}</span>
      </details>
    `
  }
  return description
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
  const seconds = timestamp - utc_seconds;
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
      <div class="col border rounded node-info" style="min-height: 8rem; width: 16rem;">
        <h5 class="card-title py-0 mt-1 mb-0">
          <span class="mx-2 mt-1 d-inline-block text-truncate" style="max-width: 15rem;">
            <img class="invert" src="static/img/node.svg" height=28 alt="Node symbol">
            ${d.name}
          </span>
        </h5>
        <div class="px-2 small">
          ${d.reachable ? "": "<span class='badge text-bg-danger'>RPC unreachable</span>"}
          <span class='badge text-bg-secondary small'>${d.version.replaceAll("/", "").replaceAll("Satoshi:", "")}</span>
          <span class="badge text-bg-secondary small">tip changed ${ago(d.last_changed_timestamp)}</span>
        </div>
        
        <div class="px-2">
          ${node_description_summary(d.description)}
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
