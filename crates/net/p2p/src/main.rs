use beers_p2p::run;

#[tokio::main]
async fn main() -> eyre::Result<()> {
    run().await
}

