/* This file is part of DarkFi (https://dark.fi)
 *
 * Copyright (C) 2020-2023 Dyne.org foundation
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of the
 * License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use futures::{stream::FuturesUnordered, TryFutureExt};
use log::{debug, error, info, warn};
use rand::{prelude::IteratorRandom, rngs::OsRng};
use smol::{lock::Mutex, stream::StreamExt, Executor};
use url::Url;

use super::{
    channel::ChannelPtr,
    dnet::DnetEvent,
    hosts::{Hosts, HostsPtr},
    message::Message,
    protocol::{protocol_registry::ProtocolRegistry, register_default_protocols},
    session::{
        InboundSession, InboundSessionPtr, ManualSession, ManualSessionPtr, OutboundSession,
        OutboundSessionPtr, SeedSyncSession,
    },
    settings::{Settings, SettingsPtr},
};
use crate::{
    system::{Subscriber, SubscriberPtr, Subscription},
    Result,
};

/// Set of channels that are awaiting connection
pub type PendingChannels = Mutex<HashSet<Url>>;
/// Set of connected channels
pub type ConnectedChannels = Mutex<HashMap<Url, ChannelPtr>>;
/// Atomic pointer to the p2p interface
pub type P2pPtr = Arc<P2p>;

/// Toplevel peer-to-peer networking interface
pub struct P2p {
    /// Global multithreaded executor reference
    executor: Arc<Executor<'static>>,
    /// Channels pending connection
    pending: PendingChannels,
    /// Connected channels
    channels: ConnectedChannels,
    /// Subscriber for notifications of new channels
    channel_subscriber: SubscriberPtr<Result<ChannelPtr>>,
    /// Subscriber for stop notifications
    stop_subscriber: SubscriberPtr<()>,
    /// Known hosts (peers)
    hosts: HostsPtr,
    /// Protocol registry
    protocol_registry: ProtocolRegistry,
    /// P2P network settings
    settings: SettingsPtr,
    /// Boolean lock marking if peer discovery is active
    pub peer_discovery_running: Mutex<bool>,

    /// Reference to configured [`ManualSession`]
    session_manual: Mutex<Option<Arc<ManualSession>>>,
    /// Reference to configured [`InboundSession`]
    session_inbound: Mutex<Option<Arc<InboundSession>>>,
    /// Reference to configured [`OutboundSession`]
    session_outbound: Mutex<Option<Arc<OutboundSession>>>,

    /// Enable network debugging
    pub dnet_enabled: Mutex<bool>,
    /// The subscriber for which we can give dnet info over
    dnet_subscriber: SubscriberPtr<DnetEvent>,
}

impl P2p {
    /// Initialize a new p2p network.
    ///
    /// Initializes all sessions and protocols. Adds the protocols to the protocol
    /// registry, along with a bitflag session selector that includes or excludes
    /// sessions from seed, version, and address protocols.
    ///
    /// Creates a weak pointer to self that is used by all sessions to access the
    /// p2p parent class.
    pub async fn new(settings: Settings, executor: Arc<Executor<'static>>) -> P2pPtr {
        let settings = Arc::new(settings);

        let self_ = Arc::new(Self {
            executor,
            pending: Mutex::new(HashSet::new()),
            channels: Mutex::new(HashMap::new()),
            channel_subscriber: Subscriber::new(),
            stop_subscriber: Subscriber::new(),
            hosts: Hosts::new(settings.clone()),
            protocol_registry: ProtocolRegistry::new(),
            settings,
            peer_discovery_running: Mutex::new(false),

            session_manual: Mutex::new(None),
            session_inbound: Mutex::new(None),
            session_outbound: Mutex::new(None),

            dnet_enabled: Mutex::new(false),
            dnet_subscriber: Subscriber::new(),
        });

        let parent = Arc::downgrade(&self_);

        *self_.session_manual.lock().await = Some(ManualSession::new(parent.clone()));
        *self_.session_inbound.lock().await = Some(InboundSession::new(parent.clone()));
        *self_.session_outbound.lock().await = Some(OutboundSession::new(parent));

        register_default_protocols(self_.clone()).await;

        self_
    }

    /// Invoke startup and seeding sequence. Call from constructing thread.
    pub async fn start(self: Arc<Self>) -> Result<()> {
        debug!(target: "net::p2p::start()", "P2P::start() [BEGIN]");
        info!(target: "net::p2p::start()", "[P2P] Seeding P2P subsystem");

        // Start seed session
        let seed = SeedSyncSession::new(Arc::downgrade(&self));
        // This will block until all seed queries have finished
        seed.start().await?;

        debug!(target: "net::p2p::start()", "P2P::start() [END]");
        Ok(())
    }

    /// Reseed the P2P network.
    pub async fn reseed(self: Arc<Self>) -> Result<()> {
        debug!(target: "net::p2p::reseed()", "P2P::reseed() [BEGIN]");
        info!(target: "net::p2p::reseed()", "[P2P] Reseeding P2P subsystem");

        // Start seed session
        let seed = SeedSyncSession::new(Arc::downgrade(&self));
        // This will block until all seed queries have finished
        seed.start().await?;

        debug!(target: "net::p2p::reseed()", "P2P::reseed() [END]");
        Ok(())
    }

    /// Runs the network. Starts inbound, outbound, and manual sessions.
    /// Waits for a stop signal and stops the network if received.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        debug!(target: "net::p2p::run()", "P2P::run() [BEGIN]");
        info!(target: "net::p2p::run()", "[P2P] Running P2P subsystem");

        // First attempt any set manual connections
        let manual = self.session_manual().await;
        for peer in &self.settings.peers {
            manual.clone().connect(peer.clone()).await;
        }

        // Start the inbound session
        let inbound = self.session_inbound().await;
        inbound.clone().start().await?;

        // Start the outbound session
        let outbound = self.session_outbound().await;
        outbound.clone().start().await?;

        info!(target: "net::p2p::run()", "[P2P] P2P subsystem started");

        // Wait for stop signal
        let stop_sub = self.subscribe_stop().await;
        stop_sub.receive().await;

        info!(target: "net::p2p::run()", "[P2P] Received P2P subsystem stop signal. Shutting down.");

        // Stop the sessions
        manual.stop().await;
        inbound.stop().await;
        outbound.stop().await;

        debug!(target: "net::p2p::run()", "P2P::run() [END]");
        Ok(())
    }

    /// Subscribe to a stop signal.
    pub async fn subscribe_stop(&self) -> Subscription<()> {
        self.stop_subscriber.clone().subscribe().await
    }

    /// Stop the running P2P subsystem
    pub async fn stop(&self) {
        self.stop_subscriber.notify(()).await
    }

    /// Broadcasts a message concurrently across all active channels.
    pub async fn broadcast<M: Message>(&self, message: &M) {
        self.broadcast_with_exclude(message, &[]).await
    }

    /// Broadcasts a message concurrently across active channels, excluding
    /// the ones provided in `exclude_list`.
    pub async fn broadcast_with_exclude<M: Message>(&self, message: &M, exclude_list: &[Url]) {
        let chans = self.channels.lock().await;
        let iter = chans.values();
        let mut futures = FuturesUnordered::new();

        for channel in iter {
            if exclude_list.contains(channel.address()) {
                continue
            }

            futures.push(channel.send(message).map_err(|e| {
                (
                    format!("[P2P] Broadcasting message to {} failed: {}", channel.address(), e),
                    channel.clone(),
                )
            }));
        }

        if futures.is_empty() {
            warn!(target: "net::p2p::broadcast()", "[P2P] No connected channels found for broadcast");
            return
        }

        while let Some(entry) = futures.next().await {
            if let Err((e, chan)) = entry {
                error!(target: "net::p2p::broadcast()", "{}", e);
                self.remove(chan).await;
            }
        }
    }

    /// Check whether we're connected to a given address
    pub async fn exists(&self, addr: &Url) -> bool {
        self.channels.lock().await.contains_key(addr)
    }

    /// Add a channel to the set of connected channels
    pub(crate) async fn store(&self, channel: ChannelPtr) {
        // TODO: Check the code path for this, and potentially also insert the remote
        // into the hosts list?
        self.channels.lock().await.insert(channel.address().clone(), channel.clone());
        self.channel_subscriber.notify(Ok(channel)).await;
    }

    /// Remove a channel from the set of connected channels
    pub(crate) async fn remove(&self, channel: ChannelPtr) {
        self.channels.lock().await.remove(channel.address());
    }

    /// Add an address to the list of pending channels.
    pub(crate) async fn add_pending(&self, addr: &Url) -> bool {
        self.pending.lock().await.insert(addr.clone())
    }

    /// Remove a channel from the list of pending channels.
    pub(crate) async fn remove_pending(&self, addr: &Url) {
        self.pending.lock().await.remove(addr);
    }

    /// Return reference to connected channels map
    pub fn channels(&self) -> &ConnectedChannels {
        &self.channels
    }

    /// Retrieve a random connected channel from the
    pub async fn random_channel(&self) -> Option<ChannelPtr> {
        let channels = self.channels().lock().await;
        channels.values().choose(&mut OsRng).cloned()
    }

    /// Return an atomic pointer to the set network settings
    pub fn settings(&self) -> SettingsPtr {
        self.settings.clone()
    }

    /// Return an atomic pointer to the list of hosts
    pub fn hosts(&self) -> HostsPtr {
        self.hosts.clone()
    }

    /// Reference the global executor
    pub(super) fn executor(&self) -> Arc<Executor<'static>> {
        self.executor.clone()
    }

    /// Return a reference to the internal protocol registry
    pub fn protocol_registry(&self) -> &ProtocolRegistry {
        &self.protocol_registry
    }

    /// Get pointer to manual session
    pub async fn session_manual(&self) -> ManualSessionPtr {
        self.session_manual.lock().await.as_ref().unwrap().clone()
    }

    /// Get pointer to inbound session
    pub async fn session_inbound(&self) -> InboundSessionPtr {
        self.session_inbound.lock().await.as_ref().unwrap().clone()
    }

    /// Get pointer to outbound session
    pub async fn session_outbound(&self) -> OutboundSessionPtr {
        self.session_outbound.lock().await.as_ref().unwrap().clone()
    }

    /// Enable network debugging
    pub async fn dnet_enable(&self) {
        *self.dnet_enabled.lock().await = true;
        warn!("[P2P] Network debugging enabled!");
    }

    /// Disable network debugging
    pub async fn dnet_disable(&self) {
        *self.dnet_enabled.lock().await = false;
        warn!("[P2P] Network debugging disabled!");
    }

    /// Subscribe to dnet events
    pub async fn dnet_subscribe(&self) -> Subscription<DnetEvent> {
        self.dnet_subscriber.clone().subscribe().await
    }

    /// Send a dnet notification over the subscriber
    pub async fn dnet_notify(&self, event: DnetEvent) {
        self.dnet_subscriber.notify(event).await;
    }
}
