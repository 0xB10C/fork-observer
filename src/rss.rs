use std::fmt;
use warp::http::Response;
use warp::Filter;

use std::collections::HashMap;
use std::convert::Infallible;

use crate::types::{Caches, ChainTipStatus, Fork, NetworkJson, NodeDataJson, TipInfoJson};

const THREASHOLD_NODE_LAGGING: u64 = 3; // blocks

/// Escapes text for inclusion in XML character data or in a double-quoted
/// attribute value. Without this a node or network name containing `&` or `<`
/// makes the whole feed unparseable, and RSS readers reject it outright rather
/// than skipping the offending item.
fn escape_xml(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Builds the `<link>` (the web page this feed is about) and the `atom:link`
/// self `href` for a feed. `base_url` comes from the config and may or may not
/// carry a trailing slash, so it is normalized here.
fn feed_urls(base_url: &str, network_id: u32, feed_name: &str, src: &str) -> (String, String) {
    let base = base_url.trim_end_matches('/');
    (
        format!("{}/?network={}&src={}", base, network_id, src),
        format!("{}/rss/{}/{}.xml", base, network_id, feed_name),
    )
}

pub fn with_rss_base_url(
    base_url: String,
) -> impl Filter<Extract = (String,), Error = Infallible> + Clone {
    warp::any().map(move || base_url.clone())
}

// A RSS item.
struct Item {
    title: String,
    description: String,
    guid: String,
}

impl fmt::Display for Item {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            r#"
  <item>
	<title>{}</title>
	<description>{}</description>
	<guid isPermaLink="false">{}</guid>
  </item>"#,
            escape_xml(&self.title),
            escape_xml(&self.description),
            escape_xml(&self.guid),
        )
    }
}

// An RSS channel.
struct Channel {
    title: String,
    description: String,
    link: String,
    items: Vec<Item>,
    href: String,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            r#"<channel>
  <title>{}</title>
  <description>{}</description>
  <link>{}</link>
  <atom:link href="{}" rel="self" type="application/rss+xml" />
  {}
</channel>"#,
            escape_xml(&self.title),
            escape_xml(&self.description),
            escape_xml(&self.link),
            escape_xml(&self.href),
            self.items.iter().map(|i| i.to_string()).collect::<String>(),
        )
    }
}

// An RSS feed.
struct Feed {
    channel: Channel,
}

impl fmt::Display for Feed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            r#"<?xml version="1.0" encoding="UTF-8" ?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">
{}
</rss>
"#,
            self.channel
        )
    }
}

impl From<Fork> for Item {
    fn from(fork: Fork) -> Self {
        Item {
            title: format!(
                "{} at height {}",
                if fork.children.len() <= 2 {
                    "Fork"
                } else {
                    "Multi-fork"
                },
                fork.common.height,
            ),
            description: format!(
                "There are {} blocks building on-top of block {}.",
                fork.children.len(),
                fork.common.header.block_hash()
            ),
            guid: fork.common.header.block_hash().to_string(),
        }
    }
}

impl From<(&TipInfoJson, &Vec<NodeDataJson>)> for Item {
    fn from(invalid_block: (&TipInfoJson, &Vec<NodeDataJson>)) -> Self {
        let mut nodes = invalid_block.1.clone();
        nodes.sort_by_key(|a| a.id);

        Item {
            title: format!("Invalid block at height {}", invalid_block.0.height,),
            description: format!(
                "Invalid block {} at height {} seen by node{}: {}",
                invalid_block.0.hash,
                invalid_block.0.height,
                if invalid_block.1.len() > 1 { "s" } else { "" },
                nodes
                    .iter()
                    .map(|node| format!("{} (id={})", node.display_name(), node.id))
                    .collect::<Vec<String>>()
                    .join(", "),
            ),
            guid: invalid_block.0.hash.clone(),
        }
    }
}

pub async fn forks_response(
    network_id: u32,
    caches: Caches,
    network_infos: Vec<NetworkJson>,
    base_url: String,
) -> Result<impl warp::Reply, Infallible> {
    let caches_locked = caches.lock().await;
    match caches_locked.get(&network_id) {
        Some(cache) => {
            let mut network_name = "";
            if let Some(network) = network_infos
                .iter()
                .filter(|net| net.id == network_id)
                .collect::<Vec<&NetworkJson>>()
                .first()
            {
                network_name = &network.name;
            }

            let (link, href) = feed_urls(&base_url, network_id, "forks", "forks-rss");
            let feed = Feed {
                channel: Channel {
                    title: format!("Recent Forks - {}", network_name),
                    description: format!(
                        "Recent forks that occured on the Bitcoin {} network",
                        network_name
                    )
                    .to_string(),
                    link,
                    href,
                    items: cache.forks.iter().map(|f| f.clone().into()).collect(),
                },
            };

            Ok(Response::builder()
                .header("content-type", "application/rss+xml")
                .body(feed.to_string()))
        }
        None => Ok(Ok(response_unknown_network(network_infos))),
    }
}

impl Item {
    pub fn lagging_node_item(node: &NodeDataJson, height: u64) -> Item {
        Item {
            title: format!("Node '{}' is lagging behind", node.display_name()),
            description: format!(
                "The node's active tip is on height {}, while other nodes consider a block with a height at least {} blocks higher their active tip. The node might still be synchronizing with the network or stuck.",
                height,
                THREASHOLD_NODE_LAGGING,
            ),
            guid: format!("lagging-node-{}-on-{}", node.display_name(), height),
        }
    }

    pub fn unreachable_node_item(node: &NodeDataJson) -> Item {
        Item {
            title: format!(
                "Node '{}' (id={}) is unreachable",
                node.display_name(),
                node.id
            ),
            description: format!(
                "The RPC server of this node is not reachable. The node might be offline or there might be other networking issues. The nodes tip data was last updated at timestamp {} (zero indicates never).",
                node.last_changed_timestamp,
            ),
            guid: format!("unreachable-node-{}-last-{}", node.id, node.last_changed_timestamp),
        }
    }
}

pub async fn lagging_nodes_response(
    network_id: u32,
    caches: Caches,
    network_infos: Vec<NetworkJson>,
    base_url: String,
) -> Result<impl warp::Reply, Infallible> {
    let caches_locked = caches.lock().await;
    match caches_locked.get(&network_id) {
        Some(cache) => {
            let mut network_name = "";
            if let Some(network) = network_infos
                .iter()
                .filter(|net| net.id == network_id)
                .collect::<Vec<&NetworkJson>>()
                .first()
            {
                network_name = &network.name;
            }

            let mut lagging_nodes: Vec<Item> = vec![];
            if cache.node_data.len() > 1 {
                let nodes_with_active_height: Vec<(&NodeDataJson, u64)> = cache
                    .node_data
                    .values()
                    .map(|node| {
                        (
                            node,
                            node.tips
                                .iter()
                                .rfind(|tip| tip.status == "active")
                                .unwrap_or(&TipInfoJson {
                                    height: 0,
                                    status: "active".to_string(),
                                    hash: "dummy".to_string(),
                                })
                                .height,
                        )
                    })
                    .collect();
                let max_height: u64 = *nodes_with_active_height
                    .iter()
                    .map(|(_, height)| height)
                    .max()
                    .unwrap_or(&0);
                for (node, height) in nodes_with_active_height.iter() {
                    if height + THREASHOLD_NODE_LAGGING < max_height {
                        lagging_nodes.push(Item::lagging_node_item(node, *height));
                    }
                }
            }

            let (link, href) = feed_urls(&base_url, network_id, "lagging", "lagging-rss");
            let feed = Feed {
                channel: Channel {
                    title: format!("Lagging nodes on {}", network_name),
                    description: format!(
                        "List of nodes that are more than 3 blocks behind the chain tip on the {} network.",
                        network_name
                    )
                    .to_string(),
                    link,
                    href,
                    items: lagging_nodes,
                },
            };

            Ok(Response::builder()
                .header("content-type", "application/rss+xml")
                .body(feed.to_string()))
        }
        None => Ok(Ok(response_unknown_network(network_infos))),
    }
}

pub async fn invalid_blocks_response(
    network_id: u32,
    caches: Caches,
    network_infos: Vec<NetworkJson>,
    base_url: String,
) -> Result<impl warp::Reply, Infallible> {
    let caches_locked = caches.lock().await;

    match caches_locked.get(&network_id) {
        Some(cache) => {
            let mut network_name = "";
            if let Some(network) = network_infos
                .iter()
                .filter(|net| net.id == network_id)
                .collect::<Vec<&NetworkJson>>()
                .first()
            {
                network_name = &network.name;
            }

            let mut invalid_blocks_to_node_id: HashMap<TipInfoJson, Vec<NodeDataJson>> =
                HashMap::new();
            for node in cache.node_data.values() {
                for tip in node.tips.iter() {
                    if tip.status == ChainTipStatus::Invalid.to_string() {
                        invalid_blocks_to_node_id
                            .entry(tip.clone())
                            .and_modify(|k| k.push(node.clone()))
                            .or_insert(vec![node.clone()]);
                    }
                }
            }

            let mut invalid_blocks: Vec<(&TipInfoJson, &Vec<NodeDataJson>)> =
                invalid_blocks_to_node_id.iter().collect();
            invalid_blocks.sort_by_key(|b| std::cmp::Reverse(b.0.height));
            let (link, href) = feed_urls(&base_url, network_id, "invalid", "invalid-rss");
            let feed = Feed {
                channel: Channel {
                    title: format!("Invalid Blocks - {}", network_name),
                    description: format!(
                        "Recent invalid blocks on the Bitcoin {} network",
                        network_name
                    ),
                    link,
                    href,
                    items: invalid_blocks
                        .iter()
                        .map(|(tipinfo, nodes)| (*tipinfo, *nodes).into())
                        .collect::<Vec<Item>>(),
                },
            };

            Ok(Response::builder()
                .header("content-type", "application/rss+xml")
                .body(feed.to_string()))
        }
        None => Ok(Ok(response_unknown_network(network_infos))),
    }
}

pub async fn unreachable_nodes_response(
    network_id: u32,
    caches: Caches,
    network_infos: Vec<NetworkJson>,
    base_url: String,
) -> Result<impl warp::Reply, Infallible> {
    let caches_locked = caches.lock().await;

    match caches_locked.get(&network_id) {
        Some(cache) => {
            let mut network_name = "";
            if let Some(network) = network_infos
                .iter()
                .filter(|net| net.id == network_id)
                .collect::<Vec<&NetworkJson>>()
                .first()
            {
                network_name = &network.name;
            }

            let unreachable_node_items: Vec<Item> = cache
                .node_data
                .values()
                .filter(|node| !node.reachable)
                .map(Item::unreachable_node_item)
                .collect();
            let (link, href) = feed_urls(&base_url, network_id, "unreachable", "unreachable-nodes");
            let feed = Feed {
                channel: Channel {
                    title: format!("Unreachable nodes - {}", network_name),
                    description: format!(
                        "Nodes on the {} network that can't be reached",
                        network_name
                    ),
                    link,
                    href,
                    items: unreachable_node_items,
                },
            };

            Ok(Response::builder()
                .header("content-type", "application/rss+xml")
                .body(feed.to_string()))
        }
        None => Ok(Ok(response_unknown_network(network_infos))),
    }
}

pub fn response_unknown_network(network_infos: Vec<NetworkJson>) -> Response<String> {
    let avaliable_networks = network_infos
        .iter()
        .map(|net| format!("{} ({})", net.id, net.name))
        .collect::<Vec<String>>();

    Response::builder()
        .status(404)
        .header("content-type", "text/plain")
        .body(format!(
            "Unknown network. Avaliable networks are: {}.",
            avaliable_networks.join(", ")
        ))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    // A node name an operator could plausibly pick that breaks unescaped XML.
    const HOSTILE: &str = r#"A & B <main> "primary" 'alt'"#;

    // Every `&` in well-formed XML must start an entity reference. Scanning for
    // that catches an unescaped `&` anywhere in the document, which is what
    // makes RSS readers drop the whole feed.
    fn every_ampersand_is_an_entity(xml: &str) -> bool {
        xml.match_indices('&').all(|(i, _)| {
            ["amp;", "lt;", "gt;", "quot;", "apos;"]
                .iter()
                .any(|entity| xml[i + 1..].starts_with(entity))
        })
    }

    fn feed_with_hostile_text() -> Feed {
        Feed {
            channel: Channel {
                title: format!("Unreachable nodes - {}", HOSTILE),
                description: format!("Nodes on the {} network that can't be reached", HOSTILE),
                link: "https://example.com/?network=1".to_string(),
                href: "https://example.com/rss/1/unreachable.xml".to_string(),
                items: vec![Item {
                    title: format!("Node '{}' (id=0) is unreachable", HOSTILE),
                    description: format!("Seen by {}", HOSTILE),
                    guid: format!("unreachable-node-{}-last-0", HOSTILE),
                }],
            },
        }
    }

    #[test]
    fn escape_xml_escapes_the_five_predefined_entities() {
        assert_eq!(
            escape_xml(r#"&<>"'"#),
            "&amp;&lt;&gt;&quot;&apos;".to_string()
        );
        assert_eq!(escape_xml("nothing to do"), "nothing to do".to_string());
    }

    #[test]
    fn rendered_feed_escapes_markup_in_names() {
        let xml = feed_with_hostile_text().to_string();

        assert!(
            every_ampersand_is_an_entity(&xml),
            "unescaped ampersand in feed:\n{}",
            xml
        );
        // The raw characters must not survive into the document...
        assert!(!xml.contains("<main>"));
        assert!(!xml.contains("A & B"));
        // ...but their escaped forms must, in both channel fields and all three
        // item fields.
        assert_eq!(xml.matches("A &amp; B &lt;main&gt;").count(), 5);
    }

    #[test]
    fn feed_urls_separate_the_query_parameters_with_an_ampersand() {
        let (link, _href) = feed_urls("https://example.com/", 1, "forks", "forks-rss");

        assert_eq!(link, "https://example.com/?network=1&src=forks-rss");
    }

    #[test]
    fn feed_urls_do_not_depend_on_a_trailing_slash_in_the_base_url() {
        let with_slash = feed_urls("https://example.com/", 1, "forks", "forks-rss");
        let without_slash = feed_urls("https://example.com", 1, "forks", "forks-rss");

        assert_eq!(with_slash, without_slash);
        assert_eq!(
            with_slash,
            (
                "https://example.com/?network=1&src=forks-rss".to_string(),
                "https://example.com/rss/1/forks.xml".to_string(),
            )
        );
    }

    #[test]
    fn feed_urls_are_escaped_when_rendered() {
        let (link, href) = feed_urls("https://example.com/", 1, "forks", "forks-rss");
        let mut feed = feed_with_hostile_text();
        feed.channel.link = link;
        feed.channel.href = href;

        assert!(feed
            .to_string()
            .contains("<link>https://example.com/?network=1&amp;src=forks-rss</link>"));
    }

    #[test]
    fn empty_feed_still_carries_the_required_channel_elements() {
        let xml = Feed {
            channel: Channel {
                title: "Recent Forks - Mainnet".to_string(),
                description: "Recent forks".to_string(),
                link: "https://example.com/?network=0".to_string(),
                href: "https://example.com/rss/0/forks.xml".to_string(),
                items: vec![],
            },
        }
        .to_string();

        assert!(xml.starts_with(r#"<?xml version="1.0" encoding="UTF-8" ?>"#));
        assert!(xml.contains("<title>Recent Forks - Mainnet</title>"));
        assert!(xml.contains("<description>Recent forks</description>"));
        assert!(xml.contains("<link>https://example.com/?network=0</link>"));
        assert!(!xml.contains("<item>"));
    }
}
