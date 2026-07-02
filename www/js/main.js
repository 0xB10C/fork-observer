const getNetworks = new Request('api/networks.json');
const getInfo = new Request('api/info.json');
const changeSSE = new EventSource('api/changes');

const networkSelect = d3.select("#network")
const nodeInfoRow = d3.select("#node_infos")
const networkInfoDescription = d3.select("#network_info_description")
const networkInfoName = d3.select("#network_info_name")
const footerCustom = d3.select("#footer-custom")
const connectionStatus = d3.select("#connection-status")
const rssRecentForks = d3.select("#rss_recent_forks")
const rssInvalidBlocks = d3.select("#rss_invalid_blocks")
const rssLaggingNodes = d3.select("#rss_lagging_nodes")
const rssUnreachableNodes = d3.select("#rss_unreachable_nodes")

const SEARCH_PARAM_NETWORK = "network"

// TODO: should be queried via the API as info
const PAGE_NAME = "fork-observer"

var state_selected_network_id = 0
var state_networks = []
var state_data = {}
var update_scheduled = false

async function fetch_info() {
  console.debug("called fetch_info()")
  await fetch(getInfo)
    .then(response => response.json())
    .then(info => {
      footerCustom.html(info.footer)
    }).catch(console.error);
}

async function fetch_data() {
  console.debug("called fetch_data()")
  await fetch(`api/${state_selected_network_id}/data.json`)
    .then(response => response.json())
    .then(data => state_data = data)
    .catch(console.error);
}

async function fetch_networks() {
  console.debug("called fetch_networks()")
  await fetch(getNetworks)
    .then(response => response.json())
    .then(networks => {
      state_networks = networks.networks
      set_initial_network()
      update_network()
    }).catch(console.error);
}

function update_network() {
  console.debug("called update_network()")
  let current_network = state_networks.filter(net => net.id == state_selected_network_id)[0]
  document.title = PAGE_NAME + " - " + current_network.name;
  networkInfoName.text(current_network.name)
  networkInfoDescription.text(current_network.description)
  rssRecentForks.node().href = `rss/${current_network.id}/forks.xml`
  rssInvalidBlocks.node().href = `rss/${current_network.id}/invalid.xml`
  rssLaggingNodes.node().href = `rss/${current_network.id}/lagging.xml`
  rssUnreachableNodes.node().href = `rss/${current_network.id}/unreachable.xml`

  // Keep the URL in sync with the selected network, using the friendly slug, so
  // it can be bookmarked and shared (e.g. ?network=testnet4).
  let url = new URL(window.location)
  url.searchParams.set(SEARCH_PARAM_NETWORK, current_network.slug)
  window.history.replaceState({}, "", url)
}

function set_initial_network() {
  console.debug("called set_initial_network()")
  let url = new URL(window.location);
  let searchParams = new URLSearchParams(url.search);
  let searchParamNetwork = searchParams.get(SEARCH_PARAM_NETWORK)

  // Match the URL parameter against the network slug or, for backwards
  // compatibility, the numeric network id.
  let matched = state_networks.find(x => x.slug == searchParamNetwork || x.id == searchParamNetwork)
  if (searchParamNetwork != null && matched != undefined) {
    console.debug("Setting network to", searchParamNetwork, "based on the URL search parameter", SEARCH_PARAM_NETWORK)
    state_selected_network_id = matched.id
  } else {
    console.debug("Setting network to first network:", state_networks[0].id);
    state_selected_network_id = state_networks[0].id
  }

  networkSelect.selectAll('option')
    .data(state_networks)
    .enter()
      .append('option')
      .attr('value', d => d.id)
      .text(d => d.name)
      .property("selected", d => d.id == state_selected_network_id)
}

networkSelect.on("input", async function() {
  state_selected_network_id = networkSelect.node().value
  update_network()
  await update()
})

async function update() {
  console.debug("called update()")
  update_scheduled = false
  await fetch_data()
  await draw_nodes()
  await draw()
}

async function run() {
  console.debug("called run()")
  await fetch_networks()
  await fetch_info()
  await update()

  periodicallyRedrawTimestamps()
}

function periodicallyRedrawTimestamps() {
  setTimeout(() => {
    let ts = document.getElementsByClassName("relativeTimestamp");
    for(t of ts) {
      let timestamp = parseInt(t.dataset.timestamp)
      t.innerHTML = ago(timestamp)
    }
    periodicallyRedrawTimestamps()
  }, 10000)
}

changeSSE.addEventListener('open', () => {
  connectionStatus.style("color", "var(--tip-status-color-active)");
  connectionStatus.attr("title", "connected — receiving live updates");
});

changeSSE.addEventListener('error', (e) => {
  console.error("SSE error", e);
  connectionStatus.style("color", "var(--tip-status-color-invalid)");
  connectionStatus.attr("title", "disconnected — reconnecting…");
});

changeSSE.addEventListener('close', (e) => {
  connectionStatus.style("color", "grey");
  connectionStatus.attr("title", "connection closed");
});

// copy text to the clipboard and confirm with a short toast
function copyToClipboard(text, label) {
  navigator.clipboard.writeText(text)
    .then(() => showToast((label ? label + " " : "") + "copied to clipboard"))
    .catch(() => showToast("could not copy to clipboard"))
}

let toastTimer = null
function showToast(message) {
  let toast = document.getElementById("toast")
  if (toast == null) return
  toast.textContent = message
  toast.classList.add("toast-show")
  clearTimeout(toastTimer)
  toastTimer = setTimeout(() => toast.classList.remove("toast-show"), 1800)
}

// Escape closes any open block info boxes
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    closeAllDescriptions()
  }
})

changeSSE.addEventListener("cache_changed", (e) => {
  let data = JSON.parse(e.data)
  console.debug("server side event: the data for one of the networks changed: ", data)
  if(data.network_id == state_selected_network_id) {
    console.debug("server side event: the data for current network changed: ", data)
    if (!update_scheduled) {
      // wait for 500ms before fetching data
      // this avoid fetching data in rapid succession when a new block is found
      setTimeout(update, 500);
      update_scheduled = true;
    } else {
      console.debug("server side event: update for the current network already sheduled: ", data)
    }
  }
})


run()
