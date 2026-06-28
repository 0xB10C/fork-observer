// Block/header fork tree rendering on top of Cytoscape.js + dagre.
//
// treelayout.js turns the raw data into Cytoscape elements; this module renders
// them with the dagre layout and native node/edge styling, handles the
// orientation toggle, and shows a simple details panel when a block is tapped.
//
// Globals provided to main.js: draw(), draw_nodes(), ago().

cytoscape.use(cytoscapeDagre);

// --- helpers -----------------------------------------------------------------

function cssVar(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function escapeHtml(s) {
  return String(s)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

// --- orientation -------------------------------------------------------------

const ORIENTATION_OPTIONS = [
  { name: "left to right", value: "left-to-right" },
  { name: "bottom to top", value: "bottom-to-top" },
];
const RANK_DIR = { "left-to-right": "LR", "bottom-to-top": "BT" };

let currentOrientation = "left-to-right";
{
  const ratio =
    (window.innerWidth || document.documentElement.clientWidth) /
    (window.innerHeight || document.documentElement.clientHeight);
  if (ratio < 1) currentOrientation = "bottom-to-top";
}

// --- cytoscape instance ------------------------------------------------------

let initialDraw = true;

const cy = cytoscape({
  container: document.getElementById("drawing-area"),
  minZoom: 0.05,
  maxZoom: 2.5,
  wheelSensitivity: 0.2,
  boxSelectionEnabled: false,
  autoungrabify: true,
  style: [
    {
      selector: "node",
      style: {
        shape: "round-rectangle",
        label: "data(height)",
        width: "46px",
        height: "30px",
        "text-valign": "center",
        "text-halign": "center",
        "font-size": "11px",
        color: "#000",
        "background-color": "#fff",
        "border-width": 1,
        "border-color": cssVar("--block-to-block-link-color") || "#888",
      },
    },
    { selector: "node.status-active", style: { "border-color": cssVar("--tip-status-color-active") } },
    { selector: "node.status-invalid", style: { "border-color": cssVar("--tip-status-color-invalid") } },
    { selector: "node.status-valid-fork", style: { "border-color": cssVar("--tip-status-color-valid-fork") } },
    { selector: "node.status-valid-headers", style: { "border-color": cssVar("--tip-status-color-valid-headers") } },
    { selector: "node.status-headers-only", style: { "border-color": cssVar("--tip-status-color-headers-only") } },
    { selector: "node.tip", style: { "border-width": 4 } },
    { selector: "node.min-diff", style: { "border-color": "darksalmon", "border-width": 3 } },
    {
      selector: "node:selected",
      style: { "border-color": "#0d6efd", "border-width": 4, "background-color": "#eaf2ff" },
    },
    {
      selector: "edge",
      style: {
        width: 2,
        "line-color": cssVar("--block-to-block-link-color") || "#000",
        "curve-style": "taxi",
        "taxi-direction": currentOrientation === "bottom-to-top" ? "upward" : "rightward",
        "target-arrow-shape": "none",
        label: "data(label)",
        "font-size": "11px",
        color: cssVar("--text-color") || "#000",
        "text-background-color": cssVar("--body-bg") || "#fff",
        "text-background-opacity": 1,
        "text-background-padding": "2px",
      },
    },
    { selector: "edge.collapsed", style: { "line-style": "dashed" } },
  ],
});

function layoutOptions() {
  return {
    name: "dagre",
    rankDir: RANK_DIR[currentOrientation],
    nodeSep: 25,
    rankSep: 55,
    edgeSep: 10,
    fit: true,
    padding: 30,
    animate: !initialDraw,
    animationDuration: 500,
  };
}

// --- draw --------------------------------------------------------------------

function draw() {
  const { elements } = build_elements(state_data);
  cy.elements().remove();
  hideDetails();
  if (elements.length === 0) return;
  cy.add(elements);
  cy.style().selector("edge").style("taxi-direction", currentOrientation === "bottom-to-top" ? "upward" : "rightward").update();
  cy.layout(layoutOptions()).run();
  initialDraw = false;
}

// --- details panel -----------------------------------------------------------

const detailsPanel = document.getElementById("block-details");

cy.on("tap", "node", (evt) => showDetails(evt.target));
cy.on("tap", (evt) => {
  if (evt.target === cy) hideDetails();
});

function hideDetails() {
  if (!detailsPanel) return;
  detailsPanel.hidden = true;
  detailsPanel.innerHTML = "";
}

function copyRowHtml(label, value) {
  return (
    `<div class="bd-row" data-copy="${escapeHtml(value)}" title="click to copy">` +
    `<span class="bd-key">${label}</span>` +
    `<span class="bd-val font-monospace">${escapeHtml(value)}</span>` +
    `</div>`
  );
}

function showDetails(node) {
  if (!detailsPanel) return;
  const data = node.data();
  const raw = data.raw;

  let statusHtml = "";
  if (data.status !== "in-chain") {
    statusHtml = data.status
      .slice()
      .reverse()
      .map(
        (s) =>
          `<div class="bd-row"><span class="tip-status-color-fill-${s.status}">▆</span> ` +
          `${s.count}x ${s.status}: ${escapeHtml(s.nodes.join(", "))}</div>`
      )
      .join("");
  }

  detailsPanel.innerHTML =
    `<div class="bd-header">` +
    `<span>Header at height <span class="font-monospace">${raw.height}</span></span>` +
    `<button type="button" class="btn-close bd-close" aria-label="Close"></button>` +
    `</div>` +
    `<div class="bd-body">` +
    copyRowHtml("hash", raw.hash) +
    copyRowHtml("previous", raw.prev_blockhash) +
    copyRowHtml("merkleroot", raw.merkle_root) +
    `<div class="bd-row"><span class="bd-key">timestamp</span><span class="bd-val font-monospace">${raw.time}</span></div>` +
    `<div class="bd-row"><span class="bd-key">version</span><span class="bd-val font-monospace">0x${raw.version.toString(16)}</span></div>` +
    `<div class="bd-row"><span class="bd-key">nonce</span><span class="bd-val font-monospace">0x${raw.nonce.toString(16)}</span></div>` +
    `<div class="bd-row"><span class="bd-key">bits</span><span class="bd-val font-monospace">0x${raw.bits.toString(16)}</span></div>` +
    `<div class="bd-row"><span class="bd-key">difficulty</span><span class="bd-val font-monospace">${raw.difficulty_int}</span></div>` +
    (raw.miner ? `<div class="bd-row"><span class="bd-key">miner</span><span class="bd-val">${escapeHtml(raw.miner)}</span></div>` : "") +
    statusHtml +
    `</div>`;
  detailsPanel.hidden = false;

  detailsPanel.querySelector(".bd-close").addEventListener("click", () => {
    cy.elements().unselect();
    hideDetails();
  });
  detailsPanel.querySelectorAll(".bd-row[data-copy]").forEach((row) => {
    row.addEventListener("click", () => window.prompt("copy:", row.dataset.copy));
  });
}

// --- orientation select ------------------------------------------------------

{
  const select = document.getElementById("orientation");
  if (select) {
    select.innerHTML = ORIENTATION_OPTIONS.map(
      (o) => `<option value="${o.value}"${o.value === currentOrientation ? " selected" : ""}>${o.name}</option>`
    ).join("");
    select.addEventListener("input", () => {
      currentOrientation = select.value;
      cy.style().selector("edge").style("taxi-direction", currentOrientation === "bottom-to-top" ? "upward" : "rightward").update();
      cy.layout(layoutOptions()).run();
    });
  }
}

window.addEventListener("resize", () => cy.resize());

// --- node info panel ---------------------------------------------------------

function get_active_height_or_0(node) {
  const active = node.tips.filter((t) => t.status === "active");
  return active.length > 0 ? active[0].height : 0;
}

function get_active_hash_or_fake(node) {
  const active = node.tips.filter((t) => t.status === "active");
  return active.length > 0
    ? active[0].hash
    : "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffdead";
}

function node_description(description) {
  return `
    <p class="mb-0 text-truncate" onclick="this.style.whiteSpace = 'normal'">
      <span>${escapeHtml(description)}</span>
    </p>`;
}

function draw_nodes() {
  const container = document.getElementById("node_infos");
  if (!container) return; // e.g. the fullscreen view has no node panel
  const sorted = state_data.nodes
    .slice()
    .sort((a, b) => get_active_height_or_0(a) - get_active_height_or_0(b));
  container.innerHTML = sorted
    .map((d) => {
      const version = d.version
        .replaceAll("/", "")
        .replaceAll("Satoshi:", "")
        .replace("unknown", "(version unknown)");
      const activeHeight = get_active_height_or_0(d);
      const activeHash = get_active_hash_or_fake(d);
      return `
      <div class="row-cols-auto px-1">
        <div class="col border rounded node-info my-2" style="min-height: 8rem; width: 16rem;">
          <h5 class="card-title py-0 mt-1 mb-0">
            <span class="mx-2 mt-1 d-inline-block text-truncate" style="max-width: 15rem;">
              <img class="invert" src="static/img/node.svg" height=28 alt="Node symbol">
              ${escapeHtml(d.name)}
            </span>
          </h5>
          <div class="px-2 small">
            ${d.reachable ? "" : "<span class='badge text-bg-danger'>unreachable</span>"}
            <span class='badge text-bg-secondary small'>${escapeHtml(d.implementation)} ${escapeHtml(version)}</span>
          </div>
          <div class="px-2">${node_description(d.description)}</div>
          <div class="px-2">
            <span class="small">tip changed <span class="relativeTimestamp" data-timestamp=${d.last_changed_timestamp}>${ago(
        d.last_changed_timestamp
      )}</span></span>
          </div>
          <div class="px-2" style="background-color: hsl(${parseInt(activeHeight * 90, 10) % 360}, 50%, 75%)">
            <span class="small text-color-dark"> height: ${activeHeight}</span>
          </div>
          <div class="px-2 rounded-bottom" style="background-color: hsl(${
            (parseInt(activeHash.substring(58), 16) + 120) % 360
          }, 50%, 75%)">
            <details>
              <summary style="list-style: none;">
                <span class="small text-color-dark">tip hash: …${activeHash.substring(54, 64)}</span>
              </summary>
              <span class="small text-color-dark">${activeHash}</span>
            </details>
          </div>
        </div>
      </div>`;
    })
    .join("");
}

// --- relative time -----------------------------------------------------------

function ago(timestamp) {
  const rtf = new Intl.RelativeTimeFormat("en", { style: "narrow", numeric: "always" });
  const now = new Date();
  const utc_seconds = (now.getTime() + now.getTimezoneOffset() * 60) / 1000;
  const seconds = parseInt(timestamp - utc_seconds);
  if (seconds > -90) return rtf.format(seconds, "seconds");
  const minutes = parseInt(seconds / 60);
  if (minutes > -60) return rtf.format(minutes, "minutes");
  const hours = parseInt(minutes / 60);
  if (hours > -24) return rtf.format(hours, "hours");
  const days = parseInt(hours / 60);
  if (days > -30) return rtf.format(days, "days");
  const months = parseInt(days / 31);
  if (months > -12) return rtf.format(months, "months");
  return "a long time ago";
}
