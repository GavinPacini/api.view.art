use {
    crate::model::ChannelContent,
    std::{collections::HashMap, sync::Arc},
    tokio::sync::{
        broadcast::{Receiver, Sender},
        RwLock,
    },
};

type BroadcastItem = Option<ChannelContent>;

#[derive(Debug, Clone)]
pub struct Changes {
    channels: Arc<RwLock<HashMap<String, Sender<BroadcastItem>>>>,
}

impl Changes {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn subscribe(
        &mut self,
        player: String,
    ) -> (Sender<BroadcastItem>, Receiver<BroadcastItem>) {
        let sender = { self.channels.read().await.get(&player).cloned() };

        match sender {
            Some(channel) => (channel.clone(), channel.subscribe()),
            None => {
                let (tx, rx) = tokio::sync::broadcast::channel(20);
                self.channels.write().await.insert(player, tx.clone());
                (tx, rx)
            }
        }
    }

    pub async fn broadcast(&self, player: &str, channel_content: ChannelContent) {
        let sender = { self.channels.read().await.get(player).cloned() };

        match sender {
            Some(sender) => match sender.send(Some(channel_content)) {
                Ok(len) => {
                    tracing::debug!("sent {} to {} receivers", player, len);
                }
                Err(err) => {
                    tracing::error!("failed to send to {} receivers: {}", player, err);
                }
            },
            None => {
                tracing::debug!("no receivers for {}", player);
            }
        }
    }
}
