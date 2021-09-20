const svgheight = window.innerHeight - d3.select("body").node().getBoundingClientRect().height;
const svgwidth = d3.select("body").node().getBoundingClientRect().width;

const getBlocks = new Request('data.json');

const NODE_SIZE = 100

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

function draw(data) {

  let block_infos = data.block_infos;
  let tip_infos = data.tip_infos;

  hash_to_tipstatus = {};
  tip_infos.forEach(tip => {
    hash_to_tipstatus[tip.hash] = tip.status;
  });

  block_infos.forEach(block_info => {
    let status = hash_to_tipstatus[block_info.hash];
    block_info.status = status == undefined? "in-chain" : status;
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

  links
    .filter(d => d.target.data.data.block_height - d.source.data.data.block_height != 1)
    .append("text")
    .attr("x", d => o.x(d.target, htoi) - ((o.x(d.target, htoi) - o.x(d.source, htoi))/2))
    .attr("y", d => o.y(d.target, htoi) - ((o.y(d.target, htoi) - o.y(d.source, htoi))/2))
    .text(d => (d.target.data.data.block_height - d.source.data.data.block_height -1) + " blocks hidden" )

  // adds each node as a group
  var node = g
    .selectAll(".node")
    .data(root_node.descendants())
    .enter()
    .append("g")
    .attr("class", d => "node" + (d.children ? " node--internal" : " node--leaf"))
    .attr("transform", d => "translate(" + o.x(d, htoi) + "," + o.y(d, htoi) + ")");

  // adds the rect to the node
  node
    .append("rect")
    .attr("height", 50)
    .attr("width", 50)
    .attr("fill", d => status_to_color[d.data.data.status])
    .attr("transform", "translate(-25, -25)")
    .style("cursor", "pointer")
    .on("mouseover", (c, d, e) => {
      d3.select("#block_info").text(JSON.stringify([d.data.data, o.x(d, htoi), o.y(d, htoi)], null, 2))
    })

  // adds the text to the node
  node
    .append("text")
    .attr("dy", ".35em")
    .attr("y", 0)
    .style("text-anchor", "middle")
    .text(d => d.data.data.block_height);

  node
    .filter(d => d.data.data.status != "in-chain")
    .append("text")
    .attr("y", 25)
    .attr("x", -25)
    .style("text-anchor", "left")
    .attr("font-size", "1px")
    .text(d => "status: " + d.data.data.status);

  let offset_x = 0;
  let offset_y = 0;
  let active_tip = root_node.leaves().filter(d => d.data.data.status == "active")[0]
  if (active_tip !== undefined) {
    offset_x = o.x(active_tip, htoi);
    offset_y = o.y(active_tip, htoi);
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


async function fetch_and_draw() {
  fetch(getBlocks)
    .then(response => response.json())
    .then(data => draw(data))
    .catch(console.error);
} 

let orientationSelect = d3.select("#orientation")

orientationSelect.on("input", function() {
  o = orientations[this.value]
  fetch_and_draw()
})

o = orientations[orientationSelect.node().value]

fetch_and_draw()
