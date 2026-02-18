//! SwarmLaunchConfig implementations for config types.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::info;

use vertex_node_api::InfrastructureContext;
use vertex_swarm_api::SwarmLaunchConfig;
use vertex_tasks::NodeTaskFn;
use vertex_swarm_bandwidth::{Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{BootNode, ClientNode};
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::TopologyHandle;

use crate::build_helpers::{build_accounting, single_task};
use crate::builder_ext::log_build_start;
use crate::config::{BootnodeConfig, ClientConfig, StorerConfig};
use crate::error::SwarmNodeError;
use crate::providers::NetworkChunkProvider;
use crate::rpc::{BootnodeRpcProviders, ClientRpcProviders, StorerRpcProviders};

/// Type marker for bootnode launch.
pub struct BootnodeLaunchTypes;

impl vertex_swarm_api::SwarmPrimitives for BootnodeLaunchTypes {
    type Spec = Arc<Spec>;
    type Identity = Arc<Identity>;
}

impl vertex_swarm_api::SwarmNetworkTypes for BootnodeLaunchTypes {
    type Topology = TopologyHandle<Arc<Identity>>;
}

/// Type marker for client node launch.
pub struct ClientLaunchTypes;

impl vertex_swarm_api::SwarmPrimitives for ClientLaunchTypes {
    type Spec = Arc<Spec>;
    type Identity = Arc<Identity>;
}

impl vertex_swarm_api::SwarmNetworkTypes for ClientLaunchTypes {
    type Topology = TopologyHandle<Arc<Identity>>;
}

impl vertex_swarm_api::SwarmClientTypes for ClientLaunchTypes {
    type Accounting = ClientAccounting<
        Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>,
        FixedPricer<Arc<Spec>>,
    >;
}

/// Type marker for storer node launch.
pub struct StorerLaunchTypes;

impl vertex_swarm_api::SwarmPrimitives for StorerLaunchTypes {
    type Spec = Arc<Spec>;
    type Identity = Arc<Identity>;
}

impl vertex_swarm_api::SwarmNetworkTypes for StorerLaunchTypes {
    type Topology = TopologyHandle<Arc<Identity>>;
}

impl vertex_swarm_api::SwarmClientTypes for StorerLaunchTypes {
    type Accounting = ClientAccounting<
        Arc<Accounting<DefaultBandwidthConfig, Arc<Identity>>>,
        FixedPricer<Arc<Spec>>,
    >;
}

#[async_trait]
impl SwarmLaunchConfig for BootnodeConfig {
    type Types = BootnodeLaunchTypes;
    type Providers = BootnodeRpcProviders<Arc<Identity>>;
    type Error = SwarmNodeError;

    async fn build(self, _ctx: &dyn InfrastructureContext) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Bootnode", self.spec(), self.network());

        let node = BootNode::builder(self.identity().clone())
            .build(self.network())
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let providers = BootnodeRpcProviders::new(topology);

        let task = single_task(move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "BootNode error");
            }
        });

        info!("Bootnode built successfully");
        Ok((task, providers))
    }
}

#[async_trait]
impl SwarmLaunchConfig for ClientConfig {
    type Types = ClientLaunchTypes;
    type Providers = ClientRpcProviders<Arc<Identity>, NetworkChunkProvider<Arc<Identity>>>;
    type Error = SwarmNodeError;

    async fn build(self, ctx: &dyn InfrastructureContext) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Client", self.spec(), self.network());

        // TODO: wire accounting into node when supported
        let _accounting = build_accounting(self.spec().clone(), self.identity(), self.bandwidth().clone());

        let (node, client_service, client_handle) = ClientNode::builder(self.identity().clone())
            .build(self.network())
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let chunk_provider = NetworkChunkProvider::new(client_handle, topology.clone());
        let providers = ClientRpcProviders::new(topology, chunk_provider);

        // Spawn client service as independent task with graceful shutdown
        ctx.executor().spawn_service("client_service", client_service);

        // Return node task - it will be spawned by the caller
        let task = single_task(move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "ClientNode error");
            }
        });

        info!("Client node built successfully");
        Ok((task, providers))
    }
}

#[async_trait]
impl SwarmLaunchConfig for StorerConfig {
    type Types = StorerLaunchTypes;
    type Providers = StorerRpcProviders<Arc<Identity>>;
    type Error = SwarmNodeError;

    async fn build(self, ctx: &dyn InfrastructureContext) -> Result<(NodeTaskFn, Self::Providers), Self::Error> {
        log_build_start("Storer", self.spec(), self.network());

        // TODO: wire accounting into node when supported
        let _accounting = build_accounting(self.spec().clone(), self.identity(), self.bandwidth().clone());

        // TODO: build storer-specific components
        let _ = self.local_store();
        let _ = self.storage();

        // Build as ClientNode for now (storer components not yet implemented)
        let (node, client_service, _client_handle) = ClientNode::builder(self.identity().clone())
            .build(self.network())
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let providers = StorerRpcProviders::new(topology);

        // Spawn client service as independent task with graceful shutdown
        ctx.executor().spawn_service("client_service", client_service);

        // Return node task - it will be spawned by the caller
        let task = single_task(move |shutdown| async move {
            if let Err(e) = node.start_and_run(shutdown).await {
                tracing::error!(error = %e, "StorerNode error");
            }
        });

        info!("Storer node built successfully");
        Ok((task, providers))
    }
}
