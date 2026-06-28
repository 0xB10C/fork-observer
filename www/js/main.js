const getNetworks = new Request("api/networks.json");
const getInfo = new Request("api/info.json");
const changeSSE = new EventSource("api/changes");

const networkSelect = document.getElementById("network");
const networkInfoDescription = document.getElementById("network_info_description");
const networkInfoName = document.getElementById("network_info_name");
const footerCustom = document.getElementById("footer-custom");
const connectionStatus = document.getElementById("connection-status");
const rssRecentForks = document.getElementById("rss_recent_forks");
const rssInvalidBlocks = document.getElementById("rss_invalid_blocks");
const rssLaggingNodes = document.getElementById("rss_lagging_nodes");
const rssUnreachableNodes = document.getElementById("rss_unreachable_nodes");

const SEARCH_PARAM_NETWORK = "network";

// TODO: should be queried via the API as info
const PAGE_NAME = "fork-observer";

var state_selected_network_id = 0;
var state_networks = [];
var state_data = {};
var update_scheduled = false;

async function fetch_info() {
  console.debug("called fetch_info()");
  await fetch(getInfo)
    .then((response) => response.json())
    .then((info) => {
      if (footerCustom) footerCustom.innerHTML = info.footer;
    })
    .catch(console.error);
}

async function fetch_data() {
  console.debug("called fetch_data()");
  await fetch(`api/${state_selected_network_id}/data.json`)
    .then((response) => response.json())
    .then((data) => (state_data = data))
    .catch(console.error);
}

async function fetch_networks() {
  console.debug("called fetch_networks()");
  await fetch(getNetworks)
    .then((response) => response.json())
    .then((networks) => {
      state_networks = networks.networks;
      set_initial_network();
      update_network();
    })
    .catch(console.error);
}

function update_network() {
  console.debug("called update_network()");
  let current_network = state_networks.filter((net) => net.id == state_selected_network_id)[0];
  document.title = PAGE_NAME + " - " + current_network.name;
  if (networkInfoName) networkInfoName.textContent = current_network.name;
  if (networkInfoDescription) networkInfoDescription.textContent = current_network.description;
  if (rssRecentForks) rssRecentForks.href = `rss/${current_network.id}/forks.xml`;
  if (rssInvalidBlocks) rssInvalidBlocks.href = `rss/${current_network.id}/invalid.xml`;
  if (rssLaggingNodes) rssLaggingNodes.href = `rss/${current_network.id}/lagging.xml`;
  if (rssUnreachableNodes) rssUnreachableNodes.href = `rss/${current_network.id}/unreachable.xml`;
}

function set_initial_network() {
  console.debug("called set_initial_network()");
  let url = new URL(window.location);
  let searchParams = new URLSearchParams(url.search);
  let searchParamNetwork = searchParams.get(SEARCH_PARAM_NETWORK);

  if (searchParamNetwork != null && state_networks.filter((x) => x.id == searchParamNetwork).length > 0) {
    console.debug("Setting network to", searchParamNetwork, "based on the URL search parameter", SEARCH_PARAM_NETWORK);
    state_selected_network_id = searchParamNetwork;
  } else {
    console.debug("Setting network to first network:", state_networks[0].id);
    state_selected_network_id = state_networks[0].id;
  }

  if (networkSelect) {
    networkSelect.innerHTML = state_networks
      .map(
        (d) =>
          `<option value="${d.id}"${d.id == state_selected_network_id ? " selected" : ""}>${d.name}</option>`
      )
      .join("");
  }
}

if (networkSelect) {
  networkSelect.addEventListener("input", async function () {
    state_selected_network_id = networkSelect.value;
    update_network();
    await update();
  });
}

async function update() {
  console.debug("called update()");
  update_scheduled = false;
  await fetch_data();
  draw_nodes();
  draw();
}

async function run() {
  console.debug("called run()");
  await fetch_networks();
  await fetch_info();
  await update();

  periodicallyRedrawTimestamps();
}

function periodicallyRedrawTimestamps() {
  setTimeout(() => {
    let ts = document.getElementsByClassName("relativeTimestamp");
    for (const t of ts) {
      let timestamp = parseInt(t.dataset.timestamp);
      t.innerHTML = ago(timestamp);
    }
    periodicallyRedrawTimestamps();
  }, 10000);
}

changeSSE.addEventListener("open", () => {
  if (connectionStatus) connectionStatus.style.color = "green";
});

changeSSE.addEventListener("error", (e) => {
  console.error("SSE error", e);
  if (connectionStatus) connectionStatus.style.color = "red";
});

changeSSE.addEventListener("close", () => {
  if (connectionStatus) connectionStatus.style.color = "grey";
});

changeSSE.addEventListener("cache_changed", (e) => {
  let data = JSON.parse(e.data);
  console.debug("server side event: the data for one of the networks changed: ", data);
  if (data.network_id == state_selected_network_id) {
    console.debug("server side event: the data for current network changed: ", data);
    if (!update_scheduled) {
      // wait for 500ms before fetching data
      // this avoids fetching data in rapid succession when a new block is found
      setTimeout(update, 500);
      update_scheduled = true;
    } else {
      console.debug("server side event: update for the current network already scheduled: ", data);
    }
  }
});

run();
