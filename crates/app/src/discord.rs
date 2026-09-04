//! Discord Rich Presence: shows "Clipping <game>" in the user's Discord
//! profile while the buffer or a recording runs, the way Medal does. Talks
//! to the local Discord client over its IPC pipe on a background thread so
//! a missing or slow Discord never touches the UI.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use discord_rich_presence::activity::{Activity, Timestamps};
use discord_rich_presence::{DiscordIpc, DiscordIpcClient};
use openclips_core::config::DiscordConfig;
use tracing::{debug, info, warn};

const RETRY_AFTER: Duration = Duration::from_secs(15);
const IDLE_TICK: Duration = Duration::from_secs(5);

/// What the presence should say right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresenceState {
    pub config: DiscordConfig,
    pub game: Option<String>,
    pub buffering: bool,
    pub recording: bool,
}

impl PresenceState {
    fn wanted(&self) -> bool {
        self.config.enabled
            && !self.config.client_id.trim().is_empty()
            && (self.buffering || self.recording)
    }

    fn details(&self) -> String {
        match (&self.game, self.config.show_game) {
            (Some(game), true) => format!("Clipping {game}"),
            _ => "Clipping".to_owned(),
        }
    }

    fn status(&self) -> &'static str {
        if self.recording {
            "Recording"
        } else {
            "Replay buffer on"
        }
    }
}

pub struct DiscordPresence {
    sender: Sender<PresenceState>,
}

impl DiscordPresence {
    pub fn start() -> Self {
        let (sender, receiver) = channel();
        let spawned = std::thread::Builder::new()
            .name("discord-presence".to_owned())
            .spawn(move || run(receiver));
        if let Err(err) = spawned {
            warn!("Discord presence thread could not start: {err}");
        }
        Self { sender }
    }

    /// Cheap to call on every status tick; the thread ignores repeats.
    pub fn update(&self, state: PresenceState) {
        let _ = self.sender.send(state);
    }
}

struct Connection {
    client: DiscordIpcClient,
    client_id: String,
}

fn run(receiver: Receiver<PresenceState>) {
    let mut current = PresenceState::default();
    let mut connection: Option<Connection> = None;
    let mut shown: Option<(String, &'static str)> = None;
    let mut since: Option<i64> = None;
    let mut next_attempt = Instant::now();
    let mut failed_once = false;

    loop {
        match receiver.recv_timeout(IDLE_TICK) {
            Ok(state) => current = state,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if !current.wanted() {
            if let Some(conn) = connection.as_mut()
                && shown.is_some()
            {
                let _ = conn.client.clear_activity();
            }
            shown = None;
            since = None;
            if !current.config.enabled
                && let Some(mut conn) = connection.take()
            {
                let _ = conn.client.close();
            }
            continue;
        }

        let client_id = current.config.client_id.trim().to_owned();
        if connection
            .as_ref()
            .is_some_and(|c| c.client_id != client_id)
            && let Some(mut conn) = connection.take()
        {
            let _ = conn.client.close();
            shown = None;
        }
        if connection.is_none() {
            if Instant::now() < next_attempt {
                continue;
            }
            let mut client = DiscordIpcClient::new(&client_id);
            match client.connect() {
                Ok(()) => {
                    info!("connected to Discord for rich presence");
                    failed_once = false;
                    connection = Some(Connection { client, client_id });
                }
                Err(err) => {
                    if !failed_once {
                        debug!("Discord is not reachable ({err}); retrying quietly");
                        failed_once = true;
                    }
                    next_attempt = Instant::now() + RETRY_AFTER;
                    continue;
                }
            }
        }

        let start = *since.get_or_insert_with(now_unix);
        let signature = (current.details(), current.status());
        if shown.as_ref() == Some(&signature) {
            continue;
        }
        let activity = Activity::new()
            .details(signature.0.as_str())
            .state(signature.1)
            .timestamps(Timestamps::new().start(start));
        let result = connection
            .as_mut()
            .map(|c| c.client.set_activity(activity))
            .unwrap_or(Ok(()));
        match result {
            Ok(()) => shown = Some(signature),
            Err(err) => {
                debug!("Discord presence update failed ({err}); reconnecting later");
                connection = None;
                shown = None;
                next_attempt = Instant::now() + RETRY_AFTER;
            }
        }
    }

    if let Some(mut conn) = connection {
        let _ = conn.client.clear_activity();
        let _ = conn.client.close();
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_text_follows_settings() {
        let mut state = PresenceState {
            config: DiscordConfig {
                client_id: "123".to_owned(),
                ..DiscordConfig::default()
            },
            game: Some("Roblox".to_owned()),
            buffering: true,
            recording: false,
        };
        assert!(state.wanted());
        assert_eq!(state.details(), "Clipping Roblox");
        assert_eq!(state.status(), "Replay buffer on");
        state.config.show_game = false;
        assert_eq!(state.details(), "Clipping");
        state.recording = true;
        assert_eq!(state.status(), "Recording");
        state.buffering = false;
        assert!(state.wanted());
        state.recording = false;
        assert!(!state.wanted());
        state.buffering = true;
        state.config.enabled = false;
        assert!(!state.wanted());
    }
}
