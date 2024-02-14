use std::fmt;
use warp::http::Response;
use warp::Filter;

use std::collections::HashMap;
use std::convert::Infallible;

use crate::types::{Caches, ChainTipStatus, Fork, NetworkJson, NodeDataJson, TipInfoJson};

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
            self.title, self.description, self.guid,
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
            self.title,
            self.description,
            self.link,
            self.href,
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
                fork.common.header.block_hash().to_string()
            ),
            guid: fork.common.header.block_hash().to_string(),
        }
    }
}

impl From<(&TipInfoJson, &Vec<NodeDataJson>)> for Item {
    fn from(invalid_block: (&TipInfoJson, &Vec<NodeDataJson>)) -> Self {
        let mut nodes = invalid_block.1.clone();
        nodes.sort_by(|a, b| a.id.cmp(&b.id));

        Item {
            title: format!("Invalid block at height {}", invalid_block.0.height,),
            description: format!(
                "Invalid block {} at height {} seen by node{}: {}",
                invalid_block.0.hash,
                invalid_block.0.height,
                if invalid_block.1.len() > 1 { "s" } else { "" },
                nodes
                    .iter()
                    .map(|node| format!("{} (id={})", node.name, node.id))
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

            let feed = Feed {
                channel: Channel {
                    title: format!("Recent Forks - {}", network_name),
                    description: format!(
                        "Recent forks that occured on the Bitcoin {} network",
                        network_name
                    )
                    .to_string(),
                    link: format!("{}?network={}", base_url.clone(), network_id),
                    href: format!("{}/rss/{}/forks.xml", base_url, network_id),
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
            invalid_blocks.sort_by(|a, b| b.0.height.cmp(&a.0.height));
            let feed = Feed {
                channel: Channel {
                    title: format!("Invalid Blocks - {}", network_name),
                    description: format!(
                        "Recent invalid blocks on the Bitcoin {} network",
                        network_name
                    ),
                    link: format!("{}?network={}", base_url.clone(), network_id),
                    href: format!("{}/rss/{}/invalid.xml", base_url, network_id),
                    items: invalid_blocks
                        .iter()
                        .map(|(tipinfo, nodes)| (*tipinfo, *nodes).into())
                        .collect::<Vec<Item>>(),
                },
            };

            return Ok(Response::builder()
                .header("content-type", "application/rss+xml")
                .body(feed.to_string()));
        }
        None => Ok(Ok(response_unknown_network(network_infos))),
    }
}

pub fn response_unknown_network(network_infos: Vec<NetworkJson>) -> Response<String> {
    let avaliable_networks = network_infos
        .iter()
        .map(|net| format!("{} ({})", net.id.to_string(), net.name))
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
