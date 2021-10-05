const svgheight = window.innerHeight - d3.select("body").node().getBoundingClientRect().height;
const svgwidth = d3.select("body").node().getBoundingClientRect().width;

const getNetworks = new Request('networks.json');

const NODE_SIZE = 125

const orientationSelect = d3.select("#orientation")
const networkSelect = d3.select("#network")

const orientations = {
  "bottom-to-top": {
    x: (d, _) => d.x,
    y: (d, htoi) => -htoi[d.data.data.block_height] * NODE_SIZE,
    linkDir: (htoi) => d3.linkVertical().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
  },
  "left-to-right": {
    x: (d, htoi) => htoi[d.data.data.block_height] * NODE_SIZE,
    y: (d, _) => d.x,
    linkDir: (htoi) => d3.linkHorizontal().x(d => o.x(d, htoi)).y(d => o.y(d, htoi)),
  },
};

let o = orientations["left-to-right"];

const status_to_color = {
  "active": "lime",
  "invalid": "fuchsia",
  "valid-fork": "cyan",
  "valid-headers": "red",
  "in-chain": "lightgray",
  "headers-only": "yellow",
}

var state_selected_network_id = 0
var state_networks = []
var state_data = {}

function draw() {
  data = state_data
  let block_infos = data.block_infos;
  let tip_infos = data.tip_infos;
  let node_infos = data.nodes;

  nodeid_to_node = {}
  for (const value of node_infos) {
    nodeid_to_node[value.id] = value
  }

  hash_to_tipstatus = {}
  tip_infos.forEach(tip => {
   if (!(tip.hash in hash_to_tipstatus)) {
     hash_to_tipstatus[tip.hash] = {}
   }
   if (!(tip.status in hash_to_tipstatus[tip.hash])) {
     hash_to_tipstatus[tip.hash][tip.status] = { status: tip.status, count: 0, nodes: []  }
   }
   hash_to_tipstatus[tip.hash][tip.status].count++
   hash_to_tipstatus[tip.hash][tip.status].nodes.push(nodeid_to_node[tip.node])

  });

  block_infos.forEach(block_info => {
    let status = hash_to_tipstatus[block_info.hash];
    block_info.status = status == undefined? "in-chain" : Object.values(status)
    block_info.is_tip = status != undefined
  })

  let min_height = Math.min(...block_infos.map(d => d.block_height))
  var treeData = d3
    .stratify()
    .id(function (d) {
      return d.hash;
    })
    .parentId(function (d) {
      // d3js requires the first prev block hash to be null
      return (d.block_height == min_height ? null : d.prev)
    })(block_infos);

  collapseLinearChainsOfBlocks(treeData, 4)

  let unique_heights = Array.from(new Set(treeData.descendants().map(d => parseInt(d.data.block_height)))).sort((a, b) =>  a - b );
  let htoi = {}; // height to array index map
  for (let index = 0; index < unique_heights.length; index++) {
    const height = unique_heights[index];
    htoi[height] = index;
  }

  let treemap = gen_treemap(o, tip_infos.length, unique_heights);

  // assigns the data to a hierarchy using parent-child relationships
  // and maps the node data to the tree layout
  var root_node = treemap(d3.hierarchy(treeData));

  var svg = d3
    .select("#drawing-area")
    .attr("width", "100%")
    .attr("height", svgheight)
    .style("border", "1px solid")

  svg.selectAll("*").remove()

  // append a 'group' element to 'svg' and
  var g = svg
      .append("g")
      .attr("transform", "translate(0, 0)");

  // adds the links between the nodes
  var links = g
    .selectAll(".link")
    .data(root_node.links())
    .enter()

  links.append("path")
    .attr("class", "link")
    .attr("d", o.linkDir(htoi))
    .attr("stroke-dasharray", d => d.target.data.data.block_height - d.source.data.data.block_height == 1 ? "0" : "4 5")

  var link_texts_hidden_blocks = links
    .filter(d => d.target.data.data.block_height - d.source.data.data.block_height != 1)
    .append("text")
    .style("text-anchor", "middle")
    .style("font-size", "12px")
    .attr("x", d => o.x(d.target, htoi) - ((o.x(d.target, htoi) - o.x(d.source, htoi))/2))
    .attr("y", d => o.y(d.target, htoi) - ((o.y(d.target, htoi) - o.y(d.source, htoi))/2))

  link_texts_hidden_blocks.append("tspan")
    .text(d => (d.target.data.data.block_height - d.source.data.data.block_height -1))
    .attr("dy", "-1.3em")
  link_texts_hidden_blocks.append("tspan")
    .text("blocks" )
    .attr("dy", "1.1em")
    .attr("x", d => o.x(d.target, htoi) - ((o.x(d.target, htoi) - o.x(d.source, htoi))/2))
  link_texts_hidden_blocks.append("tspan")
    .text("hidden")
    .attr("x", d => o.x(d.target, htoi) - ((o.x(d.target, htoi) - o.x(d.source, htoi))/2))
    .attr("dy", "1.3em")

  // adds each block as a group
  var blocks = g
    .selectAll(".block")
    .data(root_node.descendants())
    .enter()
    .append("g")
    .attr("class", d => "block" + (d.children ? " block--internal" : " block--leaf"))
    .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")");

  // adds a rect for each block
  blocks
    .append("rect")
    .attr("height", 50)
    .attr("width", 50)
    .attr("fill", "white")
    .attr("stroke", "black")
    .attr("stroke-width", "1")
    .attr("transform", "translate(-25, -25)")
    .style("cursor", "pointer")
    .on("mouseover", (c, d, e) => {
      let parentElement = d3.select(c.target.parentElement)
      // clean up
    parentElement.selectAll(".block-description").remove()
      let descText = parentElement.append("text").attr("class", "block-description")//.text(JSON.stringify(d.data.data, null, 2))
      descText.append("tspan")
        .text("block hash: " + d.data.data.hash)
      descText.append("tspan")
        .attr("dy", "1em")
        .attr("x", "0")
        .text("previous block: " + d.data.data.prev)
      descText.append("tspan")
        .attr("dy", "1em")
        .attr("x", "0")
        .text("height: " + d.data.data.block_height)
      d.data.data.status.forEach(status => {
          descText.append("tspan")
            .text(status.count + "x " + status.status + ": " + status.nodes.map(n => n.name).join(", "))
            .attr("dy", "1em")
            .attr("x", "0")
        })
      })

  // adds the text to the blocks
  blocks
    .append("text")
    .attr("dy", ".35em")
    .attr("y", 0)
    .style("text-anchor", "middle")
    .text(d => d.data.data.block_height);

  var node_groups = blocks
    .filter(d => d.data.data.status != "in-chain")
    .append("g")
    .selectAll("g")
    .data(d => d.data.data.status)
    .join("g")
    .attr("class", d => "node-indicator status-"+d.status)

  node_groups.append("circle")
    .attr("r", 8)
    .attr("cy", -24)
    .attr("cx", (d, i) => 24 - i*16)
    .attr("fill", d => status_to_color[d.status])
    .attr("stroke", "#0005")
    .attr("stroke-width", "1px")

  node_groups.append("text")
    .attr("dy", -20)
    .attr("dx", (d, i) => 24 - i*16)
    .style("text-anchor", "middle")
    .text(d => d.count)

  let offset_x = 0;
  let offset_y = 0;
  let max_height = Math.max(...block_infos.map(d => d.block_height))
  let max_height_tip = root_node.leaves().filter(d => d.data.data.block_height == max_height)[0]
  if (max_height_tip !== undefined) {
    offset_x = o.x(max_height_tip, htoi);
    offset_y = o.y(max_height_tip, htoi);
  }

  // enables zoom and panning
  const zoom = d3.zoom().scaleExtent([0.5, 1.5]).on( "zoom", e => g.attr("transform", e.transform) )
  svg.call(zoom)
  zoom.translateTo(svg, offset_x, offset_y, [svgwidth/2,svgheight/2]); 
}

// recursivly collapses linear branches of blocks longer than x,
// starting from node until all tips are reached.
function collapseLinearChainsOfBlocks(node, x) {
  if (node.children != undefined) {
    for (let index = 0; index < node.children.length; index++) {
      const descendant = node.children[index];
      let nextForkOrTip = findNextForkOrTip(descendant)
      let distance_between_blocks = nextForkOrTip.data.block_height - descendant.data.block_height
      if (distance_between_blocks > x) {
        descendant._children = descendant.children;
        descendant.children = [nextForkOrTip.parent];
      }
      collapseLinearChainsOfBlocks(nextForkOrTip, x)
    }
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
    }).catch(console.error);
} 

async function fetch_data() {
  await fetch('data.json?network='+networkSelect.node().value)
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
  await fetch_data()
  await draw()
})

o = orientations[orientationSelect.node().value]

async function run() {
  await fetch_networks()
  await fetch_data()
  await draw()
}

run()

