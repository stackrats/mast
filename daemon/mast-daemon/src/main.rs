//! Headless daemon: the full engine (observation + mutation ownership)
//! served over the per-user socket, no UI attached.

use std::sync::Arc;

use mast_engine::{Engine, EngineConfig, EngineDeps, RealConnector, RealLifecycleRunner};
use mast_project::MetadataStore;

#[tokio::main]
async fn main() {
    let store = match MetadataStore::open(MetadataStore::default_dir()) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("cannot open mast metadata: {e}");
            std::process::exit(2);
        }
    };
    let engine = Engine::new(
        EngineConfig::default(),
        EngineDeps {
            connector: Arc::new(RealConnector),
            store,
            process_env: std::env::vars().collect(),
            runner: Arc::new(RealLifecycleRunner),
            ownership: mast_engine::acquire_ownership(None),
        },
    );
    if engine.snapshot().read_only {
        eprintln!("another mast instance owns mutation — refusing to start a second daemon");
        std::process::exit(1);
    }
    engine.start();
    let path = mast_daemon::default_socket_path();
    if let Err(e) = mast_daemon::serve(engine, &path).await {
        eprintln!("daemon failed: {e}");
        std::process::exit(1);
    }
}
