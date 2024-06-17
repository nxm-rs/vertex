#![allow(missing_docs)]

use beers_p2p::run;

fn main() {
    use beers::cli::Cli;
    // use bee_node::BeeNode;

    beers::sigsegv_handler::install();

    // Enable backtraces unless a RUST_BACKTRACE value has already been explicitly set.
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    // if let Err(err) = Cli::parse_args().run(|builder, _| async {
    //     // let handle = builder.launch_node(BeersNode::default()).await?;
    //     // handle.node_exit_future.await
    // }) {
    //     eprintln!("Error: {}", err);
    //     std::process::exit(1);
    // }
    // let launcher = || ;
    
    if let Err(err) = Cli::parse_args().run() {
        eprintln!("Error: {}", err);
        std::process::exit(1);
    }
}
