use thiserror::Error;
use turbopath::AbsoluteSystemPathBuf;

use crate::{config, daemon, engine, opts, package_graph, run::scope, task_graph, task_hash};

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to open graph file {0}")]
    OpenGraphFile(std::io::Error, AbsoluteSystemPathBuf),
    #[error("failed to produce graph output")]
    GraphOutput(std::io::Error),
    #[error("failed to process environment variables")]
    Env(regex::Error),
    #[error(transparent)]
    Builder(#[from] engine::BuilderError),
    #[error(transparent)]
    Opts(#[from] opts::Error),
    #[error(transparent)]
    PackageJson(#[from] turborepo_repository::package_json::Error),
    #[error(transparent)]
    Config(#[from] config::Error),
    #[error(transparent)]
    PackageGraphBuilder(#[from] package_graph::builder::Error),
    #[error(transparent)]
    DaemonConnector(#[from] daemon::DaemonConnectorError),
    #[error(transparent)]
    Cache(#[from] turborepo_cache::CacheError),
    #[error(transparent)]
    Path(#[from] turbopath::PathError),
    #[error(transparent)]
    Scope(#[from] scope::ResolutionError),
    #[error(transparent)]
    TaskHash(#[from] task_hash::Error),
    #[error(transparent)]
    Visitor(#[from] task_graph::VisitorError),
    // Temporary variant for transition to thiserror
    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}
