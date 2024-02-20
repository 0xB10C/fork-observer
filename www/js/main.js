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
}

function set_initial_network() {
  console.debug("called set_initial_network()")
  let url = new URL(window.location);
  let searchParams = new URLSearchParams(url.search);
  let searchParamNetwork = searchParams.get(SEARCH_PARAM_NETWORK)

  if (searchParamNetwork != null && state_networks.filter(x => x.id == searchParamNetwork).length > 0) {
    console.debug("Setting network to", searchParamNetwork, "based on the URL search parameter", SEARCH_PARAM_NETWORK)
    state_selected_network_id = searchParamNetwork
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
  await fetch_data()
  await draw_nodes()
  await draw()
}

async function run() {
  console.debug("called run()")
  await fetch_networks()
  await fetch_info()
  await update()
}

changeSSE.addEventListener('open', () => {
  connectionStatus.style("color", "green");
});

changeSSE.addEventListener('error', (e) => {
  console.error("SSE error", e);
  connectionStatus.style("color", "red");
});

changeSSE.addEventListener('close', (e) => {
  connectionStatus.style("color", "grey");
});

changeSSE.addEventListener("tip_changed", (e) => {
  let data = JSON.parse(e.data)
  console.debug("server side event: the tip of one of the networks changed: ", data)
  if(data.network_id == state_selected_network_id) {
    console.debug("server side event: the tip of the currently displayed network changed: ", data)
    // HACK: wait for 1 second before fetching data
    // this gives the backend time to set the miner
    setTimeout(update, 1000);
  }
})

run()
