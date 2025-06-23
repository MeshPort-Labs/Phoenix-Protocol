use clap::Parser;
use phoenix_protocol::mesh::node::PhoenixNode;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
struct Args {
    #[arg(short, long, default_value = "4001")]
    port: u16,

    #[arg(short, long, default_value = "0")]
    shard_id: usize,

    #[arg(short, long, default_value = "unknown")]
    name: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let args = Args::parse();

    let topic_str = "phoenix-topic";
    let node = PhoenixNode::new(topic_str, args.shard_id, args.name).await?;
    node.run(topic_str).await?;

    Ok(())
}
