use {
    crate::PlaylistData,
    std::{collections::HashMap, sync::Arc},
    tokio::sync::{
        broadcast::{Receiver, Sender},
        RwLock,
    },
};

#[derive(Debug, Clone)]
pub struct Changes {
    channels: Arc<RwLock<HashMap<String, Sender<PlaylistData>>>>,
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
    ) -> (Sender<PlaylistData>, Receiver<PlaylistData>) {
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

    pub async fn broadcast(&self, player: &str, playlist: PlaylistData) {
        let sender = { self.channels.read().await.get(player).cloned() };

        match sender {
            Some(sender) => match sender.send(playlist) {
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
