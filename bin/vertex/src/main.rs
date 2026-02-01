//! Vertex Swarm node binary.

mod cli;

use vertex_node_builder::NodeBuilder;
use vertex_swarm_builder::{SwarmNodeBuilder, node_type};
use vertex_swarm_primitives::SwarmNodeType;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    cli::run(|ctx, args| async move {
        match ctx.config.protocol.node_type {
            SwarmNodeType::Client => {
                NodeBuilder::new()
                    .with_launch_context(
                        ctx.base.executor.clone(),
                        ctx.base.dirs.clone(),
                        ctx.config.infra.api.clone(),
                    )
                    .with_protocol(
                        SwarmNodeBuilder::<node_type::Client>::new(&ctx, &args.swarm).build(),
                    )
                    .launch()
                    .await?
                    .wait_for_shutdown()
                    .await;
            }
            SwarmNodeType::Storer => {
                unimplemented!("storer node not yet implemented")
            }
            SwarmNodeType::Bootnode => {
                NodeBuilder::new()
                    .with_launch_context(
                        ctx.base.executor.clone(),
                        ctx.base.dirs.clone(),
                        ctx.config.infra.api.clone(),
                    )
                    .with_protocol(
                        SwarmNodeBuilder::<node_type::Bootnode>::new(&ctx, &args.swarm).build(),
                    )
                    .launch()
                    .await?
                    .wait_for_shutdown()
                    .await;
            }
        }
        Ok(())
    })
    .await
}
