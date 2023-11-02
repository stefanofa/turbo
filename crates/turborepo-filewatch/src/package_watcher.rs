//! This module hosts the `PackageWatcher` type, which is used to watch the
//! filesystem for changes to packages.

use std::{
    collections::HashMap,
    future::IntoFuture,
    sync::{Arc, Mutex},
};

use notify::Event;
use tokio::{
    join,
    sync::{
        broadcast::{self, error::RecvError},
        oneshot,
    },
};
use turbopath::AbsoluteSystemPathBuf;
use turborepo_discovery::{PackageDiscovery, RootWorkspaceData};
use turborepo_repository::package_manager::{self, Error, PackageManager, WorkspaceGlobs};

use crate::NotifyError;

pub struct PackageWatcher {
    // _exit_ch exists to trigger a close on the receiver when an instance
    // of this struct is dropped. The task that is receiving events will exit,
    // dropping the other sender for the broadcast channel, causing all receivers
    // to be notified of a close.
    _exit_tx: oneshot::Sender<()>,
    _handle: tokio::task::JoinHandle<()>,
    package_data: Arc<Mutex<HashMap<AbsoluteSystemPathBuf, RootWorkspaceData>>>,
}

impl PackageWatcher {
    /// Creates a new package watcher whose current package data can be queried.
    pub fn new<T: PackageDiscovery + Send + 'static>(
        root: AbsoluteSystemPathBuf,
        recv: broadcast::Receiver<Result<Event, NotifyError>>,
        discovery: T,
    ) -> Result<Self, package_manager::Error> {
        let (exit_tx, exit_rx) = oneshot::channel();
        let package_data: Arc<Mutex<_>> = Default::default();
        let subscriber = Subscriber::new(exit_rx, root, package_data.clone(), recv, discovery)?;
        let handle = tokio::spawn(subscriber.watch());
        Ok(Self {
            _exit_tx: exit_tx,
            _handle: handle,
            package_data,
        })
    }

    pub async fn get_package_data(&self) -> Vec<RootWorkspaceData> {
        self.package_data
            .lock()
            .expect("not poisoned")
            .clone()
            .into_values()
            .collect()
    }
}

/// The underlying task that listens to file system events and updates the
/// internal package state.
struct Subscriber<T: PackageDiscovery> {
    exit_rx: oneshot::Receiver<()>,
    filter: WorkspaceGlobs,
    recv: broadcast::Receiver<Result<Event, NotifyError>>,
    package_data: Arc<Mutex<HashMap<AbsoluteSystemPathBuf, RootWorkspaceData>>>,
    manager: PackageManager,
    repo_root: AbsoluteSystemPathBuf,
    discovery: T,
}

impl<T: PackageDiscovery + Send + 'static> Subscriber<T> {
    fn new(
        exit_rx: oneshot::Receiver<()>,
        repo_root: AbsoluteSystemPathBuf,
        package_data: Arc<Mutex<HashMap<AbsoluteSystemPathBuf, RootWorkspaceData>>>,
        recv: broadcast::Receiver<Result<Event, NotifyError>>,
        discovery: T,
    ) -> Result<Self, Error> {
        let manager = PackageManager::get_package_manager(&repo_root, None)?;

        Ok(Self {
            exit_rx,
            filter: manager.get_workspace_globs(&repo_root)?,
            package_data,
            recv,
            manager,
            repo_root,
            discovery,
        })
    }

    async fn watch(mut self) {
        // initialize the contents
        self.rediscover_packages().await;

        // respond to changes
        loop {
            tokio::select! {
                _ = &mut self.exit_rx => return,
                file_event = self.recv.recv().into_future() => match file_event {
                    Ok(Ok(event)) => self.handle_file_event(event).await,
                    // if we get an error, we need to re-discover the packages
                    Ok(Err(_)) => self.rediscover_packages().await,
                    Err(RecvError::Closed) => return,
                    // if we end up lagging, warn and rediscover packages
                    Err(RecvError::Lagged(count)) => {
                        tracing::warn!("lagged behind {count} processing file watching events");
                        self.rediscover_packages().await;
                    },
                }
            }
        }
    }

    async fn handle_file_event(&mut self, file_event: Event) {
        let glob_source_change = self.manager.workspace_glob_source(&self.repo_root);

        tracing::trace!("file event: {:?}", file_event);

        // if it is the package manager, update the glob list
        let changed = if file_event
            .paths
            .iter()
            .any(|p| glob_source_change.as_path().eq(p))
        {
            let new_filter = self
                .manager
                .get_workspace_globs(&self.repo_root)
                // under some saving strategies a file can be totally empty for a moment
                // during a save. these strategies emit multiple events and so we can
                // a previous or subsequent event in the 'cluster' will still trigger
                .unwrap_or_else(|_| self.filter.clone());

            let changed = self.filter != new_filter;
            self.filter = new_filter;
            changed
        } else {
            false
        };

        if changed {
            // if the glob list has changed, do a recursive walk and replace
            self.rediscover_packages().await;
        } else {
            // if a path is not a valid utf8 string, it is not a valid path, so ignore
            for path in file_event
                .paths
                .iter()
                .filter_map(|p| p.as_os_str().to_str())
            {
                let path_file =
                    AbsoluteSystemPathBuf::new(path).expect("watched paths are absolute");

                // the path to the workspace this file is in is the parent
                let path_workspace = path_file
                    .parent()
                    .expect("watched paths will not be at the root")
                    .to_owned();

                let is_workspace = match self
                    .filter
                    .target_is_workspace(&self.repo_root, &path_workspace)
                {
                    Ok(is_workspace) => is_workspace,
                    Err(e) => {
                        // this will only error if `repo_root` is not an anchor of `path_workspace`.
                        // if we hit this case, we can safely ignore it
                        tracing::debug!("yielded path not in workspace: {:?}", e);
                        continue;
                    }
                };

                if is_workspace {
                    tracing::debug!("tracing file in package: {:?}", path_file);
                    let package_json = path_workspace.join_component("package.json");
                    let turbo_json = path_workspace.join_component("turbo.json");

                    let (package_exists, turbo_exists) = join!(
                        tokio::fs::try_exists(&package_json),
                        tokio::fs::try_exists(&turbo_json)
                    );

                    let mut data = self.package_data.lock().expect("not poisoned");
                    if let Ok(true) = package_exists {
                        data.insert(
                            path_workspace,
                            RootWorkspaceData {
                                package_json,
                                turbo_json: turbo_exists.unwrap_or_default().then_some(turbo_json),
                            },
                        );
                    } else {
                        data.remove(&path_workspace);
                    }
                }
            }
        }
    }

    async fn rediscover_packages(&mut self) {
        tracing::debug!("rediscovering packages");
        if let Ok(data) = self.discovery.discover_packages().await {
            let workspace = data
                .into_iter()
                .map(|p| (p.package_json.parent().expect("non-root").to_owned(), p))
                .collect();
            let mut data = self.package_data.lock().expect("not poisoned");
            *data = workspace;
        } else {
            tracing::error!("error discovering packages");
        }
    }
}

#[cfg(test)]
mod test {}
