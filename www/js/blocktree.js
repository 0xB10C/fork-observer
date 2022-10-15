const getNetworks = new Request('api/networks.json');
const getInfo = new Request('api/info.json');
const changeSSE = new EventSource('api/changes');

const NODE_SIZE = 100
const MAX_USIZE = 18446744073709551615;
const PAGE_NAME = "fork-observer"

const orientationSelect = d3.select("#orientation")
const networkSelect = d3.select("#network")
const nodeInfoRow = d3.select("#node_infos")
const networkInfoDescription = d3.select("#network_info_description")
const networkInfoName = d3.select("#network_info_name")
const footerCustom = d3.select("#footer-custom")

const orientations = {
  "bottom-to-top": {
    x: (d, _) => d.x,
    y: (d, htoi) => -htoi[d.data.data.height] * NODE_SIZE,
    linkDir: (htoi) => d3.linkVertical().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
    hidden_blocks_text: {offset_x: -45, offset_y: 0, anchor: "left"},
  },
  "left-to-right": {
    x: (d, htoi) => htoi[d.data.data.height] * NODE_SIZE,
    y: (d, _) => d.x,
    linkDir: (htoi) => d3.linkHorizontal().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
    hidden_blocks_text: {offset_x: 0, offset_y: 15, anchor: "middle"},
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

var state_selected_network_id = 0
var state_networks = []
var state_data = {}

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

  collapseLinearChainsOfBlocks(treeData, 3)

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
            .attr("class", "card m-0 p-0")
        let headerDiv = card.append("xhtml:div").attr("class", "card-header")
        headerDiv.append()
          .html(`<span>Header at height ${d.data.data.height}</span>`)
        headerDiv.append()
          .style("float", "right")
          .html(`<button class="btn btn-close"></button>`)
          .on("click", (c, d) => onBlockDescriptionCloseClick(c, d));
        card.append()
          .html(`
              <div class="card-body">
                <div class="container">
                  <div class="row small">
                    <div class="col small ">
                      <div class="row"><div class="col-2">hash</div><div class="col-10 font-monospace small">${d.data.data.hash}</div></div>
                      <div class="row"><div class="col-2">previous</div><div class="col-10 font-monospace small">${d.data.data.prev_blockhash}</div></div>
                      <div class="row"><div class="col-2">merkleroot</div><div class="col-10 font-monospace small">${d.data.data.merkle_root}</div></div>
                      <div class="row">
                        <div class="col-2">timestamp</div><div class="col-4">${d.data.data.time}</div>
                        <div class="col-2">version</div><div class="col-4 font-monospace">0x${d.data.data.version.toString(16)}</div>
                      </div>
                      <div class="row">
                        <div class="col-2">nonce</div><div class="col-4 font-monospace">0x${d.data.data.nonce.toString(16)}</div>
                        <div class="col-2">bits</div><div class="col-4 font-monospace">0x${d.data.data.bits.toString(16)}</div>
                      </div>
                      <div class="row"><div class="col">${status_text}</div></div>
                    </div>
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

  // text for the blocks
  blocks
    .append("text")
    .attr("dy", ".35em")
    .attr("class", "block-text")
    .text(d => d.data.data.height);

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

  zoom.scaleBy(svg, 1.2);
  let svgSize = d3.select("#drawing-area").node().getBoundingClientRect();
  zoom.translateTo(svg, offset_x, offset_y, [(svgSize.width - svgSize.x)/2, (svgSize.height - svgSize.y)/2])
}

// recursivly collapses linear branches of blocks longer than x,
// starting from the root until all tips are reached.
function collapseLinearChainsOfBlocks(node, x) {
  if (node.children) {
    node.children.forEach(child => {
      let nextForkOrTip = findNextForkOrTip(child)
      let distance_between_nodes = nextForkOrTip.depth - child.depth
      if (distance_between_nodes > x) {
        child.children[0].children = [nextForkOrTip.parent];
      }
      collapseLinearChainsOfBlocks(nextForkOrTip, x)
    })
  }
}

function findNextForkOrTip(node) {
  if (node.children == null) {
    // the node is a tip
    return node
  } else if (node.children.length > 1){
    // the node is a fork
    return node
  } else {
    for (const descendant of node) {
      if (descendant.children === undefined || descendant.children.length > 1) {
        return descendant;
      }
    }
  }
}

function gen_treemap(o, tips, unique_heights) {
  return d3.tree().size([tips, unique_heights]).nodeSize([NODE_SIZE, NODE_SIZE]);
}

async function draw_nodes() {
  nodeInfoRow.html(null);
  nodeInfoRow.selectAll('.node-info')
    .data(state_data.nodes)
    .enter()
    .append()
    .html(d => `
      <div class="col card node-info m-2" style="width: 16rem; min-height: 14rem;">
        <div class="card-body">
          <h5 class="card-title py-0 my-0">
            <img src="static/img/node.svg" height=48 alt="...">
            ${d.name}
          </h5>
          <div style="max-height: 4rem; overflow: auto;">
            <span class="card-text" style="max-height: 2rem;">${d.description}</span>
          </div>
        </div>
        <div class="px-2">
          <span class="small muted"> version: ${d.version}
        </div>
        <div class="px-2">
          <span class="small muted"> tip data changed: ${new Date(d.last_changed_timestamp * 1000).toLocaleTimeString()}
        </div>
        <div class="px-2" style="background-color: hsl(${parseInt(d.tips.filter(tip => tip.status == "active")[0].height * 90, 10) % 360}, 50%, 75%)">
          <span class="small muted"> height: ${d.tips.filter(tip => tip.status == "active")[0].height}
        </div>
        <div class="px-2" style="background-color: hsl(${parseInt(d.tips.filter(tip => tip.status == "active")[0].hash.substring(58), 16) % 360}, 50%, 75%)">
          <details>
            <summary style="list-style: none;">
              <span class="small muted">
                tip: ${d.tips.filter(tip => tip.status == "active")[0].hash.substring(0, 10)}..${d.tips.filter(tip => tip.status == "active")[0].hash.substring(54, 64)}
              </span>
            </summary>
            <span class="small muted">
              active tip hash: ${d.tips.filter(tip => tip.status == "active")[0].hash}
            </span>
          </details>
        </div>
      </div>
    `)
}

async function fetch_networks() {
  await fetch(getNetworks)
    .then(response => response.json())
    .then(networks => {
      state_networks = networks.networks
      let first_network_id = state_networks[0].id
      networkSelect.selectAll('option')
        .data(state_networks)
        .enter()
          .append('option')
          .attr('value', d => d.id)
          .text(d => d.name)
          .property("selected", d => d.id == first_network_id)
      state_selected_network_id = state_networks[0].id
      update_network()
    }).catch(console.error);
}

async function fetch_info() {
  await fetch(getInfo)
    .then(response => response.json())
    .then(info => {
      footerCustom.html(info.footer)
    }).catch(console.error);
}

async function fetch_data() {
  await fetch('api/data.json?network='+networkSelect.node().value)
    .then(response => response.json())
    .then(data => state_data = data)
    .catch(console.error);
}

orientationSelect.on("input", async function() {
  o = orientations[this.value]
  await draw()
})

networkSelect.on("input", async function() {
  state_selected_network_id = networkSelect.node().value
  update_network()
  await update()
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

changeSSE.addEventListener("tip_changed", (e) => {
  let data = JSON.parse(e.data)
  if(data.network_id == state_selected_network_id) {
    update()
  }
})

function update_network() {
  let current_network = state_networks.filter(net => net.id == state_selected_network_id)[0]
  document.title = PAGE_NAME + " - " + current_network.name;
  networkInfoName.text(current_network.name)
  networkInfoDescription.text(current_network.description)
}

async function update() {
  await fetch_data()
  await draw_nodes()
  await draw()
}

async function run() {
  await fetch_networks()
  await fetch_info()
  await update()
}

run()

