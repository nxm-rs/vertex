//! Progressive type-state swarm builder.
//!
//! Provides a fluent builder chain that holds the infrastructure context from
//! the start, then progressively accumulates protocol components via type-state
//! transitions. DB and peer store remain internal to `build()`.
//!
//! ```text
//! SwarmProtocolBuilder           // entry: holds &ctx
//!   .with_spec(spec)          -> SwarmWithSpec
//!   .with_identity(identity)  -> SwarmWithIdentity
//!   .with_network(network)    -> SwarmBaseBuilder       (can build bootnode)
//!   .with_accounting(acct)    -> SwarmClientBuilder      (can build client)
//!   .with_storage(ls, st)     -> SwarmStorerBuilder      (can build storer)
//! ```

use std::sync::Arc;

use tracing::info;
use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::{
    SwarmAccountingConfig, SwarmIdentity, SwarmLocalStoreConfig, SwarmNetworkConfig,
    SwarmPeerConfig, SwarmRoutingConfig, SwarmStorageConfig,
};
use vertex_swarm_bandwidth::BandwidthConfig;
use vertex_swarm_identity::Identity;
use vertex_swarm_localstore::LocalStoreConfig;
use vertex_swarm_node::BootNode;
use vertex_swarm_node::args::NetworkConfig;
use vertex_swarm_redistribution::StorageConfig;
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::KademliaConfig;

use crate::error::SwarmNodeError;
use crate::handle::{BuiltBootnode, BuiltClient, BuiltNode, BuiltStorer};
use crate::launch::{
    build_client_like_node, create_peer_store, log_build_start, open_shared_database, single_task,
};
use crate::rpc::BootnodeRpcProviders;

/// Fluent transformation API for builders.
///
/// Provides combinator methods that allow conditional or arbitrary
/// transformations to be chained inline without breaking the builder flow.
pub trait BuilderExt: Sized {
    /// Apply an arbitrary transformation to this builder.
    fn apply<F>(self, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        f(self)
    }

    /// Apply a transformation only when `cond` is true; otherwise return
    /// the builder unchanged.
    fn apply_if<F>(self, cond: bool, f: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        if cond { f(self) } else { self }
    }
}

/// Entry point: protocol builder with infrastructure context.
pub struct SwarmProtocolBuilder<'ctx> {
    ctx: &'ctx dyn InfrastructureContext,
}

impl<'ctx> SwarmProtocolBuilder<'ctx> {
    /// Create a new protocol builder rooted in the given infrastructure context.
    pub fn with_context(ctx: &'ctx dyn InfrastructureContext) -> Self {
        Self { ctx }
    }

    /// Set the network specification and advance to [`SwarmWithSpec`].
    pub fn with_spec(self, spec: Arc<Spec>) -> SwarmWithSpec<'ctx> {
        SwarmWithSpec {
            ctx: self.ctx,
            spec,
        }
    }
}

/// Builder stage: context + spec.
pub struct SwarmWithSpec<'ctx> {
    ctx: &'ctx dyn InfrastructureContext,
    spec: Arc<Spec>,
}

impl<'ctx> SwarmWithSpec<'ctx> {
    /// Set the node identity and advance to [`SwarmWithIdentity`].
    pub fn with_identity<I: SwarmIdentity>(self, identity: I) -> SwarmWithIdentity<'ctx, I> {
        SwarmWithIdentity {
            ctx: self.ctx,
            spec: self.spec,
            identity,
        }
    }
}

/// Builder stage: context + spec + identity.
pub struct SwarmWithIdentity<'ctx, I: SwarmIdentity> {
    ctx: &'ctx dyn InfrastructureContext,
    spec: Arc<Spec>,
    identity: I,
}

impl<'ctx, I: SwarmIdentity> SwarmWithIdentity<'ctx, I> {
    /// Set the network configuration and advance to [`SwarmBaseBuilder`].
    pub fn with_network<N>(self, network: N) -> SwarmBaseBuilder<'ctx, I, N>
    where
        N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    {
        SwarmBaseBuilder {
            ctx: self.ctx,
            spec: self.spec,
            identity: self.identity,
            network,
        }
    }
}

/// Base builder: can build bootnode or transition to client.
pub struct SwarmBaseBuilder<'ctx, I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    ctx: &'ctx dyn InfrastructureContext,
    spec: Arc<Spec>,
    identity: I,
    network: N,
}

impl<'ctx, I, N> BuilderExt for SwarmBaseBuilder<'ctx, I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
}

impl<'ctx, I, N> SwarmBaseBuilder<'ctx, I, N>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
{
    /// Returns the network specification.
    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    /// Returns the node identity.
    pub fn identity(&self) -> &I {
        &self.identity
    }

    /// Returns the network configuration.
    pub fn network(&self) -> &N {
        &self.network
    }

    /// Add accounting configuration and advance to [`SwarmClientBuilder`].
    pub fn with_accounting<A>(self, accounting: A) -> SwarmClientBuilder<'ctx, I, N, A>
    where
        A: SwarmAccountingConfig,
    {
        SwarmClientBuilder {
            ctx: self.ctx,
            spec: self.spec,
            identity: self.identity,
            network: self.network,
            accounting,
        }
    }
}

/// Client builder: can build client or transition to storer.
pub struct SwarmClientBuilder<'ctx, I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig,
{
    ctx: &'ctx dyn InfrastructureContext,
    spec: Arc<Spec>,
    identity: I,
    network: N,
    accounting: A,
}

impl<'ctx, I, N, A> BuilderExt for SwarmClientBuilder<'ctx, I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig,
{
}

impl<'ctx, I, N, A> SwarmClientBuilder<'ctx, I, N, A>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig,
{
    /// Returns the network specification.
    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }

    /// Add storage configuration and advance to [`SwarmStorerBuilder`].
    pub fn with_storage<S, St>(
        self,
        local_store: S,
        storage: St,
    ) -> SwarmStorerBuilder<'ctx, I, N, A, S, St>
    where
        S: SwarmLocalStoreConfig,
        St: SwarmStorageConfig,
    {
        SwarmStorerBuilder {
            ctx: self.ctx,
            spec: self.spec,
            identity: self.identity,
            network: self.network,
            accounting: self.accounting,
            local_store,
            storage,
        }
    }
}

/// Storer builder: can build storer.
pub struct SwarmStorerBuilder<'ctx, I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    ctx: &'ctx dyn InfrastructureContext,
    spec: Arc<Spec>,
    identity: I,
    network: N,
    accounting: A,
    local_store: S,
    storage: St,
}

impl<'ctx, I, N, A, S, St> BuilderExt for SwarmStorerBuilder<'ctx, I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
}

impl<'ctx, I, N, A, S, St> SwarmStorerBuilder<'ctx, I, N, A, S, St>
where
    I: SwarmIdentity,
    N: SwarmNetworkConfig + SwarmPeerConfig + SwarmRoutingConfig,
    A: SwarmAccountingConfig,
    S: SwarmLocalStoreConfig,
    St: SwarmStorageConfig,
{
    /// Returns the network specification.
    pub fn spec(&self) -> &Arc<Spec> {
        &self.spec
    }
}

/// Default bootnode builder.
pub type DefaultBaseBuilder<'ctx> =
    SwarmBaseBuilder<'ctx, Arc<Identity>, NetworkConfig<KademliaConfig>>;

/// Default client builder.
pub type DefaultClientBuilder<'ctx> =
    SwarmClientBuilder<'ctx, Arc<Identity>, NetworkConfig<KademliaConfig>, BandwidthConfig>;

/// Default storer builder.
pub type DefaultStorerBuilder<'ctx> = SwarmStorerBuilder<
    'ctx,
    Arc<Identity>,
    NetworkConfig<KademliaConfig>,
    BandwidthConfig,
    LocalStoreConfig,
    StorageConfig,
>;

impl DefaultBaseBuilder<'_> {
    /// Build the bootnode.
    pub async fn build(self) -> Result<BuiltBootnode, SwarmNodeError> {
        log_build_start("Bootnode", &self.spec, &self.network);

        let db = open_shared_database(self.ctx);
        let (peer_store, score_store) = create_peer_store(&db);

        let node = BootNode::builder(self.identity.clone())
            .build(&self.network, peer_store, score_store)
            .await
            .map_err(|e| SwarmNodeError::Build(e.into()))?;

        let topology = node.topology_handle().clone();
        let providers = BootnodeRpcProviders::new(topology);

        let task = single_task(move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "BootNode error");
            }
        });

        info!("Bootnode built successfully");
        Ok(BuiltNode::new(task, providers))
    }
}

impl DefaultClientBuilder<'_> {
    /// Build the client node.
    pub async fn build(self) -> Result<BuiltClient, SwarmNodeError> {
        let (task, providers) = build_client_like_node(
            "Client",
            &self.spec,
            &self.identity,
            &self.network,
            self.accounting,
            self.ctx,
        )
        .await?;
        Ok(BuiltNode::new(task, providers))
    }
}

impl DefaultStorerBuilder<'_> {
    /// Build the storer node.
    pub async fn build(self) -> Result<BuiltStorer, SwarmNodeError> {
        // TODO: build storer-specific components
        let _ = &self.local_store;
        let _ = &self.storage;

        let (task, providers) = build_client_like_node(
            "Storer",
            &self.spec,
            &self.identity,
            &self.network,
            self.accounting,
            self.ctx,
        )
        .await?;
        Ok(BuiltNode::new(task, providers))
    }
}
