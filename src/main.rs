use clap::Parser;
use phoenix_protocol::mesh::node::PhoenixNode;
// use phoenix_protocol::crypto::threshold::ThresholdCrypto;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "4001")]
    port: u16, // placeholder if you add manual ports

    #[arg(short, long, default_value = "0")]
    shard_id: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let args = Args::parse();

    let topic_str = "phoenix-topic";
    let node = PhoenixNode::new(topic_str, args.shard_id).await?;
    node.run(topic_str).await?;

    Ok(())
}
