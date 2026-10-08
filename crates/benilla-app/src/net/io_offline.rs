//! Inert channel endpoints for the offline build; no network or worker-thread code is compiled.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use benilla_protocol::SessionEvent;
use crossbeam_channel::{Receiver, Sender};

use super::{CharRequest, ClientCommand, RealmRequest};

pub(crate) static INBOUND_PACKETS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn inbound_census() -> (u64, Option<u64>) {
    (0, None)
}

pub(crate) struct LoginRequest {
    pub(crate) user: String,
    pub(crate) pass: String,
    pub(crate) host: String,
    pub(crate) generation: u64,
}

#[derive(Default)]
pub(crate) struct PingClock {
    pub(crate) last_rtt_ms: Option<u32>,
}

impl PingClock {
    pub(crate) fn avg_latency_ms(&self) -> Option<u32> {
        None
    }
}

pub(super) struct NetHandles {
    pub(super) events: Receiver<SessionEvent>,
    #[cfg(feature = "offline")]
    pub(super) event_sender: Sender<SessionEvent>,
    pub(super) commands: Sender<ClientCommand>,
    #[cfg(feature = "offline")]
    pub(super) command_receiver: Receiver<ClientCommand>,
    pub(super) pick: Sender<CharRequest>,
    #[cfg(feature = "offline")]
    pub(super) pick_receiver: Receiver<CharRequest>,
    pub(super) realm: Sender<RealmRequest>,
    pub(super) login: Sender<LoginRequest>,
    pub(super) login_abandon: Arc<AtomicU64>,
    pub(super) ping: Arc<Mutex<PingClock>>,
}

pub(super) fn spawn_net(_connect: bool) -> NetHandles {
    let (events_tx, events) = crossbeam_channel::unbounded();
    #[cfg(feature = "offline")]
    let event_sender = events_tx.clone();
    let (commands, command_receiver) = crossbeam_channel::unbounded();
    let (pick, pick_receiver) = crossbeam_channel::unbounded();
    #[cfg(not(feature = "offline"))]
    drop((events_tx, pick_receiver, command_receiver));
    let (realm, _realm_rx) = crossbeam_channel::unbounded();
    let (login, _login_rx) = crossbeam_channel::unbounded();
    NetHandles {
        events,
        #[cfg(feature = "offline")]
        event_sender,
        commands,
        #[cfg(feature = "offline")]
        command_receiver,
        pick,
        #[cfg(feature = "offline")]
        pick_receiver,
        realm,
        login,
        login_abandon: Arc::new(AtomicU64::new(0)),
        ping: Arc::new(Mutex::new(PingClock::default())),
    }
}
