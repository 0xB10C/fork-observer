const height = window.innerHeight - d3.select("body").node().getBoundingClientRect().height;
const width = d3.select("body").node().getBoundingClientRect().width;


const orientations = {
  "bottom-to-top": {
    nodeSize: [100, 75],
    x: (d, _, offset = 0) => (width/2) - d.x + offset,
    y: (d, htoi, offset = 0) => height - htoi[d.data.data.block_height] * o.nodeSize[1] - 30 + offset,
    linkDir: (htoi, offset_x, offset_y) => d3.linkVertical().x(d => o.x(d, htoi, offset_x)).y(d => o.y(d, htoi, offset_y)),
  },
  "left-to-right": {
    nodeSize: [100, 75],
    x: (d, htoi, offset = 0) => htoi[d.data.data.block_height] * o.nodeSize[1] + offset,
    y: (d, _, offset = 0) => d.x + offset,
    linkDir: (htoi, offset_x, offset_y) => d3.linkHorizontal().x(d => o.x(d, htoi, offset_x)).y(d => o.y(d, htoi, offset_y)),
  }
};

const o = orientations["bottom-to-top"];

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
    .attr("height", height)
    .style("border", "1px solid")

  let offset_x = 0;
  let offset_y = 0;
  let active_tip = root_node.leaves().filter(d => d.data.data.status == "active")[0]
  if (active_tip !== undefined) {
    offset_x = active_tip.x;
    offset_y = active_tip.y;
  }
  
  // appends a 'group' element to 'svg'
  // moves the 'group' element to the top left margin
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
    .attr("d", o.linkDir(htoi, offset_x, offset_y))

  links
    .filter(d => d.target.data.data.block_height - d.source.data.data.block_height != 1)
    .append("text")
    .attr("x", d => o.x(d.target, htoi, offset_x) - ((o.x(d.target, htoi, offset_x) - o.x(d.source, htoi, offset_x))/2))
    .attr("y", d => o.y(d.target, htoi, offset_y) - ((o.y(d.target, htoi, offset_y) - o.y(d.source, htoi, offset_y))/2))
    .text(d => (d.target.data.data.block_height - d.source.data.data.block_height -1) + " blocks hidden" )


  // adds each node as a group
  var node = g
    .selectAll(".node")
    .data(root_node.descendants())
    .enter()
    .append("g")
    .attr("class", d => "node" + (d.children ? " node--internal" : " node--leaf"))
    .attr("transform", d => "translate(" + o.x(d, htoi, offset_x) + "," + o.y(d, htoi, offset_y) + ")");

  // adds the rect to the node
  node
    .append("rect")
    .attr("height", 50)
    .attr("width", 50)
    .attr("fill", d => status_to_color[d.data.data.status])
    .attr("transform", "translate(-25, -25)")
    .style("cursor", "pointer")
    .on("mouseover", (c, d, e) => {
      d3.select("#block_info").text(JSON.stringify(d.data.data, null, 2))
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

  // enables zoom and panning
  svg.call(d3.zoom().scaleExtent([0.5, 3]).on("zoom", e => {console.log(e); g.attr("transform", e.transform)}));
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
  return d3.tree().size([tips, unique_heights]).nodeSize(o.nodeSize);
}

const getBlocks = new Request('data.json');
fetch(getBlocks)
  .then(response => response.json())
  .then(data => {
    draw(data)
  })
  .catch(console.error);