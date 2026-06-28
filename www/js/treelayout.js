// Pure data transform for the block/header fork tree.
//
// Takes the raw `data.json` payload (header_infos + nodes) and returns a flat
// list of Cytoscape elements (nodes + edges). Long linear runs of uninteresting
// blocks are collapsed into a single edge that carries the number of hidden
// blocks. Positioning is left entirely to the Cytoscape layout (dagre) — this
// module is side-effect free and contains no rendering or layout math.

const MIN_DIFFICULTY = 1;
// usize::MAX as serialized by the backend for the root block's `prev_id`.
// Note: this exceeds Number.MAX_SAFE_INTEGER, but the JSON value and this
// literal round to the same float, so equality comparison still works.
const MAX_USIZE = 18446744073709551615;

// How many consecutive uninteresting blocks may exist before a run is collapsed.
const COLLAPSE_THRESHOLD = 4;

// When a tip is reported with several statuses across nodes, the block is
// coloured by the most significant one.
const STATUS_PRECEDENCE = ["active", "invalid", "valid-fork", "valid-headers", "headers-only"];

// --- lightweight hierarchy (replaces d3.stratify / d3.hierarchy) -------------

// A node mirrors the bits the collapse algorithm relies on: `data` (the
// header_info), `children` (array, or undefined for a leaf), `parent`, `depth`.
function build_hierarchy(header_infos) {
  const by_id = new Map();
  header_infos.forEach((hi) => by_id.set(hi.id, { data: hi, children: undefined, parent: null, depth: 0 }));

  let root = null;
  for (const node of by_id.values()) {
    if (node.data.prev_id === MAX_USIZE || !by_id.has(node.data.prev_id)) {
      root = node;
      continue;
    }
    const parent = by_id.get(node.data.prev_id);
    if (parent.children === undefined) parent.children = [];
    parent.children.push(node);
    node.parent = parent;
  }

  if (root) {
    const queue = [root];
    while (queue.length) {
      const n = queue.shift();
      if (n.children) {
        for (const c of n.children) {
          c.depth = n.depth + 1;
          queue.push(c);
        }
      }
    }
  }
  return root;
}

// Pre-order DFS list of a node and all of its descendants.
function descendants(node) {
  const out = [];
  const stack = [node];
  while (stack.length) {
    const n = stack.pop();
    out.push(n);
    if (n.children) {
      for (let i = n.children.length - 1; i >= 0; i--) stack.push(n.children[i]);
    }
  }
  return out;
}

// --- collapse logic ----------------------------------------------------------

function isInteresting(node) {
  if (node.children === undefined) return true; // tip / leaf
  if (node.children.length > 1) return true; // fork
  if (node.data.status !== "in-chain") return true; // a node's tip with a status
  return false;
}

function findNextInteresting(node) {
  if (isInteresting(node)) return node;
  for (const d of descendants(node)) {
    if (isInteresting(d)) return d;
  }
  return null;
}

// Recursively collapses linear branches longer than `x`, rewiring the second
// visible block of a run directly to the parent of the next interesting block.
// The skipped height range becomes the "blocks hidden" count on the edge.
function stripUninteresting(node, x) {
  if (!node.children) return;
  node.children.forEach((child) => {
    const nextForkOrTip = findNextInteresting(child);
    const distance = nextForkOrTip.depth - child.depth;
    if (distance > x && child.children) {
      child.children[0].children = [nextForkOrTip.parent];
    }
    stripUninteresting(nextForkOrTip, x);
  });
}

// --- tip status enrichment ---------------------------------------------------

// Annotates each header_info with `status`: either the string "in-chain" or an
// array of { status, count, nodes } describing which nodes report it as a tip.
function enrich_tip_status(header_infos, nodes) {
  const hash_to_tipstatus = {};
  nodes.forEach((node) => {
    node.tips.forEach((tip) => {
      const byHash = (hash_to_tipstatus[tip.hash] ||= {});
      const entry = (byHash[tip.status] ||= { status: tip.status, count: 0, nodes: [] });
      entry.count++;
      entry.nodes.push(node.name);
    });
  });

  header_infos.forEach((hi) => {
    const status = hash_to_tipstatus[hi.hash];
    hi.status = status === undefined ? "in-chain" : Object.values(status);
    hi.is_tip = status !== undefined;
  });
}

// The most significant status reported for a tip, used for its colour.
function primary_status(statusArr) {
  for (const s of STATUS_PRECEDENCE) {
    if (statusArr.some((x) => x.status === s)) return s;
  }
  return (statusArr[0] && statusArr[0].status) || "headers-only";
}

// --- public entry point ------------------------------------------------------

// Returns { elements, maxHeight }. `elements` is a flat array of Cytoscape
// node/edge definitions; positions are assigned later by the dagre layout.
function build_elements(data) {
  if (!data || !data.header_infos || data.header_infos.length === 0) {
    return { elements: [], maxHeight: 0 };
  }

  const header_infos = data.header_infos;
  enrich_tip_status(header_infos, data.nodes || []);

  const root = build_hierarchy(header_infos);
  if (!root) return { elements: [], maxHeight: 0 };

  stripUninteresting(root, COLLAPSE_THRESHOLD);

  const visible = descendants(root);
  const maxHeight = Math.max(...header_infos.map((hi) => hi.height));

  const nodes = visible.map((n) => {
    const hi = n.data;
    const classes = [];
    if (hi.is_tip) {
      classes.push("tip");
      classes.push("status-" + primary_status(hi.status));
    } else {
      classes.push("in-chain");
    }
    if (hi.difficulty_int === MIN_DIFFICULTY) classes.push("min-diff");
    return {
      group: "nodes",
      data: {
        id: hi.hash,
        height: hi.height,
        miner: hi.miner,
        status: hi.status, // "in-chain" or [{status,count,nodes:[name,...]}]
        isTip: hi.is_tip,
        raw: hi, // full header for the detail panel
      },
      classes: classes.join(" "),
    };
  });

  const edges = [];
  visible.forEach((n) => {
    if (!n.children) return;
    n.children.forEach((child) => {
      const gap = child.data.height - n.data.height;
      const hidden = gap > 1 ? gap - 1 : 0;
      edges.push({
        group: "edges",
        data: {
          id: `e-${n.data.hash}-${child.data.hash}`,
          source: n.data.hash,
          target: child.data.hash,
          hidden,
          label: hidden > 0 ? `${hidden} blocks hidden` : "",
        },
        classes: hidden > 0 ? "collapsed" : "",
      });
    });
  });

  return { elements: [...nodes, ...edges], maxHeight };
}
