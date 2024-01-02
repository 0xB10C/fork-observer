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


A sample Bitcoin Core configuration could contain the following:

```config
rpcauth=forkobserver:<password generated with rpcauth.py>

rpcwhitelist=forkobserver:getchaintips,getblockheader,getblockhash,getblock
# OR if you're fine with exposing getnetworkinfo
# rpcwhitelist=forkobserver:getchaintips,getblockheader,getblockhash,getblock,getnetworkinfo

# If you want to access *your* node's RPC interface via e.g. a wireguard tunnel
# from some *other host*.
# rpcbind=<your-wireguard-IP> # e.g. rpcbind=10.10.0.3 (local)
# rpcallowip=<other-host-IP> # e.g. rpcallowip=10.10.0.2 (remote)
```

[rpcauth.py]: https://github.com/bitcoin/bitcoin/tree/master/share/rpcauth
[online version]: https://jlopp.github.io/bitcoin-core-rpc-auth-generator/
