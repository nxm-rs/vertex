//! SwarmLaunchConfig implementations for config types.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::info;

use vertex_node_api::NodeContext;
use vertex_swarm_api::{NodeTask, SwarmLaunchConfig};
use vertex_swarm_bandwidth::{Accounting, ClientAccounting, DefaultBandwidthConfig, FixedPricer};
use vertex_swarm_identity::Identity;
use vertex_swarm_node::{BootNode, ClientNode};
use vertex_swarm_spec::Spec;
use vertex_swarm_topology::TopologyHandle;
use vertex_tasks::SpawnableTask;

use crate::build_helpers::{build_accounting, dual_task, single_task};
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

    async fn build(self, _ctx: &NodeContext) -> Result<(NodeTask, Self::Providers), Self::Error> {
        log_build_start("Bootnode", self.spec(), self.network());

        let node = BootNode::builder(self.identity().clone())
            .build(self.network())
            .await
            .map_err(|e| SwarmNodeError::Build(e.to_string()))?;

        let topology = node.topology_handle().clone();
        let providers = BootnodeRpcProviders::new(topology);

        let task = single_task(async move {
            node.into_task().await;
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

    async fn build(self, _ctx: &NodeContext) -> Result<(NodeTask, Self::Providers), Self::Error> {
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

        let task = dual_task("Node", node.into_task(), "Client service", client_service.run());

        info!("Client node built successfully");
        Ok((task, providers))
    }
}

#[async_trait]
impl SwarmLaunchConfig for StorerConfig {
    type Types = StorerLaunchTypes;
    type Providers = StorerRpcProviders<Arc<Identity>>;
    type Error = SwarmNodeError;

    async fn build(self, _ctx: &NodeContext) -> Result<(NodeTask, Self::Providers), Self::Error> {
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

        let task = dual_task("Node", node.into_task(), "Client service", client_service.run());

        info!("Storer node built successfully");
        Ok((task, providers))
    }
}
