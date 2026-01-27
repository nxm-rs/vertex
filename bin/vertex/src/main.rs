//! Vertex Swarm node binary.

use vertex_node_builder::NodeBuilder;
use vertex_swarm_node::{SwarmNodeBuilder, SwarmNodeType, cli, node_type};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    cli::run(|ctx, args| async move {
        match ctx.config.protocol.node_type {
            SwarmNodeType::Light => {
                NodeBuilder::new()
                    .with_launch_context(
                        ctx.base.executor.clone(),
                        ctx.base.dirs.clone(),
                        ctx.config.infra.api.clone(),
                    )
                    .with_protocol(
                        SwarmNodeBuilder::<node_type::Light>::new(&ctx, &args.swarm).build(),
                    )
                    .launch()
                    .await?
                    .wait_for_shutdown()
                    .await;
            }
            SwarmNodeType::Full => {
                unimplemented!("full node not yet implemented")
            }
            SwarmNodeType::Bootnode => {
                unimplemented!("bootnode not yet implemented")
            }
            SwarmNodeType::Publisher => {
                unimplemented!("publisher node not yet implemented")
            }
            SwarmNodeType::Staker => {
                unimplemented!("staker node not yet implemented")
            }
        }
        Ok(())
    })
    .await
}
