//! Docker runtime layer: context/endpoint resolution (ADR-0002), safe command
//! execution (plan §4), and the `RuntimeAdapter` trait with its bollard
//! implementation for *observation*. Lifecycle stays with CLI shell-outs
//! (M3) so behavior matches the developer's terminal.

pub mod adapter;
pub mod command;
pub mod endpoint;
pub mod observer;

pub use adapter::{
    BollardAdapter, CapturedLine, ContainerObservation, LogChunk, RuntimeAdapter, RuntimeEvent,
};
pub use command::{
    CommandError, CommandOutcome, CommandOutput, OutputLine, run_command, run_streaming,
    spawn_detached,
};
pub use observer::{
    CommandFinish, CommandObserver, CommandStart, register_command_observer,
};
pub use endpoint::{DockerEndpoint, EndpointSource, resolve_endpoint};

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("docker CLI failed: {0}")]
    Cli(String),
    #[error("unsupported endpoint for observation: {0} (lifecycle via CLI still works)")]
    UnsupportedEndpoint(String),
    #[error("docker API error: {0}")]
    Api(String),
    #[error(transparent)]
    Command(#[from] CommandError),
}
