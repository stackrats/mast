//! Manual check against the live registry: `cargo run -p mast-registry --example probe`.
#[tokio::main]
async fn main() {
    for repo in ["mariadb", "mysql/mysql-server", "redis", "axllent/mailpit", "ghcr.io/foo/bar"] {
        match mast_registry::fetch_versions(repo).await {
            Ok(v) => println!("{repo:32} {v:?}"),
            Err(e) => println!("{repo:32} ERR {e}"),
        }
    }
}
