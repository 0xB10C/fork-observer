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


## Connecting to a block-dn server

[block-dn] serves blockchain data (chain tip, headers, blocks) over plain
HTTP(S), designed to be easy to front with a CDN for light clients. Like
Esplora, it has no `getchaintips`-like endpoint, so fork-observer only shows
its single active tip - this is most useful alongside at least one Bitcoin
Core or btcd node on the same network. Public instances are available for
mainnet, testnet3, testnet4 and signet.

```toml
[[networks.nodes]]
id = 5
name = "block-dn.org"
description = "public block-dn instance"
rpc_host = "https://block-dn.org"
implementation = "block-dn"
```

**Limitation:** headers are read out of block-dn's `/headers/<start-height>`
files, which fork-observer assumes hold 100,000 headers each - the value the
public mainnet, testnet3, testnet4 and signet instances use. A block-dn instance
started with `--regtest` writes 2,000 headers per file instead and is not
supported.

[block-dn]: https://block-dn.org/


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

## Connecting to another fork-observer instance

A network can also import the nodes and headers of another fork-observer instance. This
is fetched via the remote instance's HTTP API every `query_interval` seconds and merged
into the local network's header tree, so the remote's nodes show up alongside the
locally configured ones, marked with a `via <name>` label. `network_id` is the id of
the network on the *remote* instance;
`node_id_offset` is added to the remote's node ids to avoid colliding with locally
configured node ids. It must be unique per remote source and larger than any node id
used in this network.

```toml
[[networks.forkobservers]]
name = "b10c's observer"
description = "Another fork-observer instance"
url = "https://fork-observer.example.com"
network_id = 1
node_id_offset = 1000
```

A node that a remote instance itself imported from yet another instance is never
re-imported - propagation stops after one hop. This makes it safe to point two
fork-observer instances at each other: each side only ever shows the other's own nodes,
rather than the two accumulating each other's imports indefinitely.

> [!NOTE]
> A remote instance serves the same stripped-down header tree it shows in its own
> frontend, not every header it knows: headers at heights it considers uninteresting
> (see `max_interesting_heights`) are not part of the response and can't be imported.
>
> Imported headers are checked to hash to the block hash the remote reports, but
> nothing beyond that is verified - heights and miners are taken at face value and
> are written to the local database permanently. Only import from an instance you
> trust as much as you'd trust one of your own nodes.

## Syncing headers from a preferred node

Every node walks the chain backwards from its own tip until it reaches a header
fork-observer already has, which with an empty database means fetching the whole
chain: a few hundred requests for a node that serves batches of headers (Bitcoin
Core with REST, block-dn), tens of thousands for a node that serves one header
per request.

The first node to finish fills the header tree for the whole network, and the
others then stop at the first header they find in it. Nodes known to be fast can
be put in front of the rest with `preferred_sync_source`:

```toml
[[networks.nodes]]
id = 5
name = "block-dn.org"
description = "public block-dn instance"
rpc_host = "https://block-dn.org"
implementation = "block-dn"
preferred_sync_source = true
```

The other nodes wait until this node finished its first fetch - or ran into an
error, which lets them go ahead too, as does a 30 minute timeout. Setting it on
more than one node makes the others wait for all of them.

## Checking the chain a node serves

A node that serves a chain other than the one its network is configured for - a
mainnet network with a node that starts serving testnet headers, for example -
shares no block with the network's header tree. Nothing then stops the backwards
header walk, so that whole foreign chain ends up in the tree and the database.
Set a network's `genesis_hash` to only fetch headers from nodes that serve the
expected chain:

```toml
[[networks]]
id = 1
name = "Mainnet"
min_fork_height = 0
max_interesting_heights = 100
genesis_hash = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
```

The hash is checked with a single request before headers are fetched, so once
per tip change. A node serving a different genesis block is logged and skipped -
its chain tips aren't shown either - and checked again a minute later, then with
a doubling delay up to about an hour. A node that can't be asked for its genesis
block hash at all is treated like any other unreachable node and still used.

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

## Activity log

fork-observer can keep a timestamped, per-node activity log recording, for
example, active tip changes, reorgs, newly appearing fork tips, invalid
blocks, nodes becoming unreachable/reachable, and nodes lagging behind their
peers. The log lives in its own SQLite database (separate from the headers
database) and is enabled by adding an
`[activity]` section to the configuration; nodes then opt in individually
with `activity_log = true` (see `config.toml.example`).

Recent events are served, newest first, via:

```
GET /api/<network_id>/activity.json?before=<id>
```

A request serves a fixed 100 events. Requests without `before` are served
from an in-memory cache of recent events; passing the smallest `id` of the
previous response as `before` paginates into the database.

Two pages in the web interface show the log:

- `/activity` lists the events of a network, newest first, filterable by
  event kind, node, and free text, and pages further into the past with the
  `before` cursor.
- `/playback` replays the events onto the header tree: it reconstructs what
  each node's tips looked like at the time of every logged event by undoing
  events from the current state backwards, and steps or plays through them.
  Because the log records changes rather than snapshots, the reconstruction
  is approximate where events are missing - a fork tip a node silently stops
  reporting produces no event, and a reorg onto a branch that was already
  known as a fork tip drops that tip instead of restoring its old status.
  Blocks that `data.json` no longer carries, because they stopped being
  interesting, are put back from the events that name them: the runs the tree
  draws as "N blocks hidden" are straight lines of blocks, one per height, so
  a block's height says where in such a run it belongs. These blocks are drawn
  flat and greyed, as only their hash and height are known.

With `retention_days` (globally, or per network via
`activity_retention_days`) configured, events older than the retention are
periodically moved into monthly `activity-archive-YYYY-MM.sqlite` files in
`archive_directory` and purged from the live database. The archive files use
the same schema as the live database, so archived events remain queryable
with regular SQLite tooling.

## Benchmarks

The functions that recompute a network's view of the header tree after a new
block (`strip_tree`, `stale_blocks`, `recent_forks` and
`sorted_interesting_heights` in `src/headertree.rs`) walk the whole header
tree, which holds close to a million headers for a mainnet instance. The
[criterion](https://github.com/bheisler/criterion.rs) benchmarks in
`benches/headertree.rs` measure them on synthetic trees of 10k, 100k and 1M
headers, with a stale block every 100 heights, for the `max_interesting_heights`
values 100 and 5000. The benchmarks in `benches/db.rs` measure the database
functions that grow with the number of stored headers (`update_miner` and
`load_treeinfos`) on an in-memory database with a million rows.

```
cargo bench                                    # everything (takes a few minutes)
cargo bench --bench db                         # only the database benchmarks
cargo bench -- 'strip_tree/.*/100000'          # only a subset, by name
cargo bench -- --save-baseline before          # store a run ...
cargo bench -- --baseline before               # ... and compare a change against it
```

Criterion writes a report to `target/criterion/report/index.html`.

## Mutation testing

[cargo-mutants](https://mutants.rs) changes the code in small ways - flipping a
comparison, dropping a match arm, returning an empty `Vec` - and runs the test
suite for each change. A mutant that no test fails on ("missed") is a piece of
logic the tests don't actually check. This complements line coverage: a line can
be executed by a test without the test asserting anything about what it does.

```
cargo mutants                                  # everything (a few minutes)
cargo mutants -f src/headertree.rs             # only one file
cargo mutants -j 8                             # in parallel
cargo mutants --in-diff <(git diff main)       # only what a branch changed
```

`.cargo/mutants.toml` configures which code is mutated: the backends,
`src/jsonrpc.rs` and `src/main.rs` are left out, as they talk to nodes or only
wire things together, and a few mutants that provably don't change behaviour
are excluded by name there.

Some mutants turn a loop into an endless one. The tests catch those by
hanging, which is reported as `TIMEOUT` rather than as caught.
