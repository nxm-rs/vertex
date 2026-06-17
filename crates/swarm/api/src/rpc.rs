//! Role trait aliases for a node's gRPC surface.
//!
//! The generic providers container (`NodeProviders<C>`) lives in
//! `vertex-swarm-builder`, where the gRPC registration impl is orphan-legal.
//! These aliases name the capability hierarchy its components type `C` walks.

use crate::{HasChunkClient, HasStore, HasTopology};

/// Role alias: a bootnode exposes topology only.
pub trait Bootnode: HasTopology {}
impl<C: HasTopology> Bootnode for C {}

/// Role alias: a client adds a chunk client on top of the bootnode surface.
pub trait Client: Bootnode + HasChunkClient {}
impl<C: Bootnode + HasChunkClient> Client for C {}

/// Role alias: a storer adds a local store on top of the client surface.
pub trait Storer: Client + HasStore {}
impl<C: Client + HasStore> Storer for C {}
