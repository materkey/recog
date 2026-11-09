//! Reading and setting the bridge's internal state, for tests and the differential
//! probe: what the Python tests reach as `bridge._connected_clients`,
//! `bridge._copilot_pending_requests`, `bridge._meeting_owner_ws` and the like.

use std::sync::Arc;

use recog_core::pyjson::{Map, Value};

use crate::bridge::Bridge;
use crate::client::{Client, ClientId, ClientSink};
use crate::copilot::{AgentMerge, TickState};

/// The set a client is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// `_connected_clients`: every socket until its hello.
    Extension,
    Subscriber,
    Channel,
    Supervisor,
}

impl Bridge {
    /// Puts a client in a set without a connection (`bridge._connected_clients.add(ws)`).
    pub fn add_client(&self, role: Role, sink: Arc<dyn ClientSink>, remote_address: &str) -> Arc<Client> {
        let client = Client::new(self.next_client_id(), sink, remote_address);
        let mut st = self.st();
        st.clients.all.insert(client.id, client.clone());
        let set = match role {
            Role::Extension => &mut st.clients.connected,
            Role::Subscriber => &mut st.clients.subscribers,
            Role::Channel => &mut st.clients.channels,
            Role::Supervisor => &mut st.clients.supervisors,
        };
        set.insert(client.id);
        client
    }

    /// Takes a client out of every set (`discard`) without the disconnect cleanup.
    pub fn remove_client(&self, id: ClientId) {
        let mut st = self.st();
        let c = &mut st.clients;
        c.connected.remove(&id);
        c.subscribers.remove(&id);
        c.channels.remove(&id);
        c.supervisors.remove(&id);
        c.timed_out.remove(&id);
        c.all.remove(&id);
    }

    /// The sets a client is in.
    #[must_use]
    pub fn roles_of(&self, id: ClientId) -> Vec<Role> {
        let st = self.st();
        let c = &st.clients;
        let mut roles = Vec::new();
        for (set, role) in [
            (&c.connected, Role::Extension),
            (&c.subscribers, Role::Subscriber),
            (&c.channels, Role::Channel),
            (&c.supervisors, Role::Supervisor),
        ] {
            if set.contains(&id) {
                roles.push(role);
            }
        }
        roles
    }

    /// The clients of a set, in connection order.
    #[must_use]
    pub fn clients_in(&self, role: Role) -> Vec<ClientId> {
        let st = self.st();
        let c = &st.clients;
        let set = match role {
            Role::Extension => &c.connected,
            Role::Subscriber => &c.subscribers,
            Role::Channel => &c.channels,
            Role::Supervisor => &c.supervisors,
        };
        set.iter().copied().collect()
    }

    /// Whether a send to the client timed out (`_timed_out_clients`).
    #[must_use]
    pub fn is_timed_out(&self, id: ClientId) -> bool {
        self.st().clients.timed_out.contains(&id)
    }

    /// `_copilot_pending_requests`, in insertion order.
    #[must_use]
    pub fn pending_requests(&self) -> Vec<(String, String)> {
        self.st().pending.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn set_pending_request(&self, request_id: &str, meeting_id: &str) {
        self.st().pending.insert(request_id.to_owned(), meeting_id.to_owned());
    }

    /// `_meeting_owner_ws`.
    #[must_use]
    pub fn meeting_owner(&self) -> Option<ClientId> {
        self.st().meeting_owner
    }

    pub fn set_meeting_owner(&self, owner: Option<ClientId>) {
        self.st().meeting_owner = owner;
    }

    /// `_copilot_tick_state`.
    #[must_use]
    pub fn tick_state(&self) -> Option<TickState> {
        self.st().tick.clone()
    }

    pub fn set_tick_state(&self, tick: Option<TickState>) {
        self.st().tick = tick;
    }

    /// The recent-transcript buffer's entries, in order.
    #[must_use]
    pub fn transcript_buffer(&self) -> Vec<Map> {
        self.st().transcript.ordered()
    }

    /// The agent-state merge's fields.
    #[must_use]
    pub fn with_agent<T>(&self, f: impl FnOnce(&mut AgentMerge) -> T) -> T {
        f(&mut self.st().agent)
    }

    /// `_clock_offset`.
    #[must_use]
    pub fn clock_offset(&self) -> f64 {
        self.st().clock_offset
    }

    pub fn set_clock_offset(&self, offset: f64) {
        self.st().clock_offset = offset;
    }

    /// `_last_seq`.
    #[must_use]
    pub fn last_seq(&self) -> Value {
        self.st().last_seq.clone()
    }

    /// `_set_manual_copilot_active(active)`.
    pub fn force_manual_copilot_active(&self, active: bool) {
        self.set_manual_copilot_active(active);
    }

    /// `_pcm_transport_stats`.
    #[must_use]
    pub fn pcm_transport_stats(&self) -> Map {
        self.st().speakers.pcm_transport_stats.clone()
    }

    /// The speaker fields, for assertions on what a frame changed.
    #[must_use]
    pub fn with_speakers<T>(&self, f: impl FnOnce(&mut crate::speakers::Speakers) -> T) -> T {
        f(&mut self.st().speakers)
    }

    /// Replaces the identity as `configure_user_profile` would, without a source.
    pub fn set_identity(&self, identity: Value) {
        self.st().identity = identity;
    }

    /// Lets the debug file be written again at once (`_last_debug_update = 0`).
    pub fn reset_debug_throttle(&self) {
        self.st().last_debug_update = 0.0;
    }

    /// The number of background tasks still running.
    #[must_use]
    pub fn background_tasks(&self) -> usize {
        self.background.lock().map(|t| t.iter().filter(|t| !t.is_finished()).count()).unwrap_or(0)
    }
}
