use std::fmt;
use warp::http::Response;

use std::convert::Infallible;

use crate::types::{Caches, DataQuery, Fork, NetworkJson};

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
	<guid>{}</guid>
  </item>"#,
            self.title, self.description, self.guid,
        )
    }
}

// An RSS channel.
struct Channel {
    title: String,
    description: String,
    items: Vec<Item>,
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            r#"<channel>
  <title>{}</title>
  <description>{}</description>
  {}
</channel>"#,
            self.title,
            self.description,
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
<rss version="2.0">

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

pub async fn forks_response(
    caches: Caches,
    network_infos: Vec<NetworkJson>,
    query: DataQuery,
) -> Result<impl warp::Reply, Infallible> {
    let network_id: u32 = query.network;

    let caches_locked = caches.lock().await;
    if let Some(cache) = caches_locked.get(&network_id) {
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
                items: cache.forks.iter().map(|f| f.clone().into()).collect(),
            },
        };

        return Ok(Response::builder()
            .header("content-type", "application/rss+xml")
            .body(feed.to_string()));
    };

    let avaliable_networks = network_infos
        .iter()
        .map(|net| format!("{} ({})", net.id.to_string(), net.name))
        .collect::<Vec<String>>();

    let avaliable_networks_string = avaliable_networks.join(", ");

    return Ok(Response::builder()
        .status(404)
        .header("content-type", "text/plain")
        .body(format!(
            "Unknown network. Avaliable networks are: {}.",
            avaliable_networks_string
        )));
}
