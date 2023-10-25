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
use turborepo_repository::package_manager::PackageManager;

#[derive(Clone)]
pub struct PackageData {
    pub package_json: AbsoluteSystemPathBuf,
    pub turbo_json: Option<AbsoluteSystemPathBuf>,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    // Repo(#[from] turborepo_repository::Error),
}

/// Defines a strategy for discovering packages on the filesystem.
pub trait PackageDiscovery {
    // desugar to assert that the future is Send
    fn discover_packages(
        &mut self,
    ) -> impl std::future::Future<Output = Result<Vec<PackageData>, Error>> + Send;
}

pub struct LocalPackageDiscovery {
    repo_root: AbsoluteSystemPathBuf,
    manager: PackageManager,
}

impl LocalPackageDiscovery {
    pub fn new(repo_root: AbsoluteSystemPathBuf) -> Self {
        let manager = PackageManager::get_package_manager(&repo_root, None).unwrap();
        Self { repo_root, manager }
    }
}

impl PackageDiscovery for LocalPackageDiscovery {
    async fn discover_packages(&mut self) -> Result<Vec<PackageData>, Error> {
        iter(self.manager.get_package_jsons(&self.repo_root).unwrap())
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
