use std::sync::Arc;

use tokio::sync::watch::Receiver;
use turbopath::AbsoluteSystemPathBuf;
use turborepo_discovery::{Error, PackageData, PackageDiscovery};

use crate::daemon::{DaemonClient, FileWatching};

pub struct DaemonPackageDiscovery<'a, C: Clone> {
    daemon: &'a mut DaemonClient<C>,
}

impl<'a, C: Clone + Send> PackageDiscovery for DaemonPackageDiscovery<'a, C> {
    async fn discover_packages(&mut self) -> Result<Vec<PackageData>, Error> {
        Ok(self
            .daemon
            .discover_packages()
            .await
            .unwrap()
            .into_iter()
            .map(|p| PackageData {
                package_json: AbsoluteSystemPathBuf::new(p.package_json).unwrap(),
                turbo_json: p.turbo_json.map(|t| AbsoluteSystemPathBuf::new(t).unwrap()),
            })
            .collect())
    }
}

/// A package discovery strategy that watches the file system for changes. Basic
/// idea:
/// - Set up a watcher on file changes on the relevant workspace file for the
///   package manager
/// - When the workspace globs change, re-discover the workspace
/// - When a package.json changes, re-discover the workspace
/// - Keep an in-memory cache of the workspace
pub struct WatchingPackageDiscovery {
    /// file watching may not be ready yet so we store a watcher
    /// through which we can get the file watching stack
    watcher: Receiver<Option<Arc<crate::daemon::FileWatching>>>,
}

impl WatchingPackageDiscovery {
    pub fn new(watcher: Receiver<Option<Arc<FileWatching>>>) -> Self {
        Self { watcher }
    }
}

impl PackageDiscovery for WatchingPackageDiscovery {
    async fn discover_packages(&mut self) -> Result<Vec<PackageData>, Error> {
        // need to clone and drop the Ref before we can await
        let watcher = {
            let watcher = self.watcher.wait_for(|opt| opt.is_some()).await.unwrap();
            watcher.as_ref().unwrap().clone()
        };
        Ok(watcher.package_watcher.get_package_data().await)
    }
}

#[cfg(test)]
mod test {
    use turbopath::AbsoluteSystemPathBuf;

    use crate::daemon::DaemonConnector;

    #[tokio::test]
    async fn test_daemon_package_discovery() {
        let connector = DaemonConnector {
            can_start_server: true,
            can_kill_server: true,
            pid_file: AbsoluteSystemPathBuf::new("/tmp/turbod/6c5948bd4171b931/turbod.pid")
                .unwrap(),
            sock_file: AbsoluteSystemPathBuf::new("/tmp/turbod/6c5948bd4171b931/turbod.sock")
                .unwrap(),
        };

        let mut client = connector.connect().await.unwrap();

        let packages = client.discover_packages().await.unwrap();

        println!("{:#?}", packages);
    }
}
