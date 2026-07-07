# fork-observer


## Connecting to a Bitcoin Core node

For getting a good overview over different chain fork on the Bitcoin network,
fork-observer ideally needs access to multiple Bitcoin Core nodes. It was
designed work with many nodes on multiple networks in parallel. Additionally,
if another party is willing to give you RPC access over e.g., an encrypted
channel like wireguard, you can add their node to your fork-observer instance.
This requires only access to three RPC calls that can be whitelisted. Note:
Don't give anyone RPC access when your node is used to handle real-world funds.
Next to Bitcoin Core wallet funds this includes funds in a Lightning node
connected to your Bitcoin Core node.

fork-observer uses the Bitcoin Core RPC interface to query information about
headers and the chain tips. The REST interface is used to query batches of
main chain (the chain leading up to the chain tip) headers. Requesting block
header batches via REST is more performant than requesting them individually
through RPC. While REST is optional, it's recommended to connect to at least
a few nodes that have the RPC interface enabled. The REST interface can be
disabled by setting `use_rest = false` in the per network node configuration
in config.toml.

It's recommended to set up a persistent Bitcoin Core RPC user for the fork-
observer. A password hash can be generated, for example, with the [rpcauth.py]
script provided by Bitcoin Core or third-party tools like jlopp's [online
version]. Compared to using cookie-based authentication, a dedicated user
enables you to limit the allowed RPCs for this user.

fork-observer needs access to the following RPCs:

- `getchaintips`: Used to query available chain tips and their status.
- `getblockhash`: Used to query a block hash given a specific height.
- `getblockheader`: Used to query (stale) block headers.
- `getnetworkinfo` (optional): Used once during start-up query the Bitcoin Core
  version. This RPC could potentially expose private information about your
  nodes connectivity.
- `getblock` (optional): Used for miner identification.
- `waitfornewblock` (optional) for faster notifications about blocks


A sample Bitcoin Core configuration could contain the following:

```config
rpcauth=forkobserver:<password generated with rpcauth.py>

rpcwhitelist=forkobserver:getchaintips,getblockheader,getblockhash,getblock
# OR if you're fine with exposing getnetworkinfo
# rpcwhitelist=forkobserver:getchaintips,getblockheader,getblockhash,getblock,getnetworkinfo,waitfornewblock

# If you want to access *your* node's RPC interface via e.g. a wireguard tunnel
# from some *other host*.
# rpcbind=<your-wireguard-IP> # e.g. rpcbind=10.10.0.3 (local)
# rpcallowip=<other-host-IP> # e.g. rpcallowip=10.10.0.2 (remote)
```

[rpcauth.py]: https://github.com/bitcoin/bitcoin/tree/master/share/rpcauth
[online version]: https://jlopp.github.io/bitcoin-core-rpc-auth-generator/

## Connecting to an Esplora REST API

Block explorers like blockstream.info are based on [esplora]. While they don't
offer a `getchaintips`-like API endpoint, it can be useful to know which block
these explorers consider to be the tip. A esplora backend can, for example, be
configured using the following "node" configuration.

**Limitation:** Esplora never reports stale/fork blocks, only the active tip.
Only add it to a network that already has at least one Bitcoin Core (or btcd)
node.

```toml
[[networks.nodes]]
id = 2
name = "blockstream.info"
description = "blockstream.info REST API"
rpc_host = "https://blockstream.info/api"
implementation = "esplora"
```

[esplora]: https://github.com/Blockstream/esplora


## Connecting to mempool.space

mempool.space additionally exposes a `getchaintips`-like `/api/v1/chain-tips`
endpoint, so a `mempoolspace` backend reports stale/fork chain tips as well as
the active one (unlike the generic `esplora` backend above, which can only
report the current active tip). Note that this endpoint is not yet stabilized
by the mempool.space project.

```toml
[[networks.nodes]]
id = 4
name = "mempool.space"
description = "mempool.space public API"
rpc_host = "https://mempool.space/api"
implementation = "mempoolspace"
```

Use `rpc_host = "https://mempool.space/testnet/api"` (or `/signet/api`) to
connect to a different network.

**Limitations:** fork branches are only fetched for the tips
`/api/v1/chain-tips` currently reports, and mempool.space serves no block data
for the `headers-only` tips among them, so this backend can't reconstruct fork
history on its own. Only add it to a network that already has at least one
Bitcoin Core (or btcd) node.

> [!NOTE]
> Loading the full header tree from a mempool.space instance is neither
> supported nor recommended. This is a public, rate-limited API with no bulk
> header endpoint, so headers are fetched one at a time.
>
> In the recommended setup this costs almost nothing: all nodes of a network
> share one header tree, so the Bitcoin Core node supplies the active chain and
> this backend stops at the first header it already knows - normally after a
> single request per new block. Backfilling a long stretch of history from
> mempool.space alone, on the other hand, is slow and will run into rate limits,
> so keep `min_fork_height` close to the current tip.


## Connecting to an Electrum server

The fork-observer tool can connect to public and private Electrum servers.
While electrum servers don't offer a `getchaintips`-like API endpoint, it can be useful to
know which blocks Electrum servers consider to be the tip. Supported are both plaintext 
`tcp://` and encrypted `ssl://` connections.

```toml
[[networks.nodes]]
id = 3
name = "Electrum Emzy"
description = "URL electrum.emzy.de:50002"
rpc_host = "ssl://electrum.emzy.de"
rpc_port = 50002
implementation = "electrum"
```

**Limitation:** Electrum has no protocol call for stale/fork block headers, so
this backend never reports forks, only the active tip. Only add it to a
network that already has at least one Bitcoin Core (or btcd) node.

## Countdown to a block height

Each network can optionally show a countdown to a specific block height (e.g. a
halving) in the frontend. At most one countdown per network; when omitted,
nothing is shown. The five blocks around the target height
(`height - 2` to `height + 2`) are always kept in the API response once mined,
regardless of `max_interesting_heights`.

```toml
[networks.countdown]
height = 1050000
label = "Halving"
```
