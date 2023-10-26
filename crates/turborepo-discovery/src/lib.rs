//! turborepo-discovery
//!
//! This package contains a number of strategies for discovering various things
//! about a workspace. These traits come with a basic implementation and some
//! adaptors that can be used to compose them together.
//!
//! This powers various intents such as 'query the daemon for this data, or
//! fallback to local discovery if the daemon is not available'. Eventually,
//! these strategies will implement some sort of monad-style composition so that
//! we can track areas of run that are performing sub-optimally.

#![feature(async_fn_in_trait)]
#![feature(return_position_impl_trait_in_trait)]

use tokio_stream::{iter, StreamExt};
use turbopath::AbsoluteSystemPathBuf;
use turborepo_repository::package_manager::{self, PackageManager};

#[derive(Clone)]
pub struct PackageData {
    pub package_json: AbsoluteSystemPathBuf,
    pub turbo_json: Option<AbsoluteSystemPathBuf>,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("discovery unavailable")]
    Unavailable,
    #[error("discovery failed")]
    Failed,
}

/// Defines a strategy for discovering packages on the filesystem.
pub trait PackageDiscovery {
    // desugar to assert that the future is Send
    fn discover_packages(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Vec<PackageData>, Error>> + Send;
}

impl<T: PackageDiscovery + Send> PackageDiscovery for Option<T> {
    async fn discover_packages(&mut self) -> Result<Vec<PackageData>, Error> {
        match self {
            Some(d) => d.discover_packages().await,
            None => Err(Error::Unavailable),
        }
    }
}

pub struct LocalPackageDiscovery {
    repo_root: AbsoluteSystemPathBuf,
    manager: PackageManager,
}

impl LocalPackageDiscovery {
    pub fn new(repo_root: AbsoluteSystemPathBuf) -> Result<Self, package_manager::Error> {
        let manager = PackageManager::get_package_manager(&repo_root, None)?;
        Ok(Self { repo_root, manager })
    }
}

impl PackageDiscovery for LocalPackageDiscovery {
    async fn discover_packages(&mut self) -> Result<Vec<PackageData>, Error> {
        iter(
            self.manager
                .get_package_jsons(&self.repo_root)
                .map_err(|e| Error::Failed)?,
        )
        .then(move |a| async {
            let potential_turbo = a.parent().expect("non-root").join_component("turbo.json");
            let potential_turbo_exists = tokio::fs::try_exists(potential_turbo.as_path()).await;

            Ok(PackageData {
                package_json: a,
                turbo_json: potential_turbo_exists
                    .ok()
                    .and_then(|pe| pe.then_some(potential_turbo)),
            })
        })
        .collect()
        .await
    }
}

pub struct FallbackPackageDiscovery<A, B> {
    a: A,
    b: B,
    timeout: std::time::Duration,
}

impl<A, B> FallbackPackageDiscovery<A, B> {
    pub fn new(a: A, b: B, timeout: std::time::Duration) -> Self {
        Self { a, b, timeout }
    }
}

impl<A: PackageDiscovery + Send, B: PackageDiscovery + Send> PackageDiscovery
    for FallbackPackageDiscovery<A, B>
{
    async fn discover_packages(&mut self) -> Result<Vec<PackageData>, Error> {
        match tokio::time::timeout(self.timeout, self.a.discover_packages()).await {
            Ok(Ok(packages)) => Ok(packages),
            Ok(Err(err1)) => match self.b.discover_packages().await {
                Ok(packages) => Ok(packages),
                // if the backup is unavailable, return the original error
                Err(Error::Unavailable) => Err(err1),
                Err(err2) => Err(err2),
            },
            Err(_) => self.b.discover_packages().await,
        }
    }
}
