use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use turbo_tasks::{
    graph::{AdjacencyMap, GraphTraversal},
    primitives::StringVc,
    TryJoinIterExt, ValueToString, ValueToStringVc,
};

use crate::{
    asset::{Asset, AssetVc, AssetsVc},
    issue::IssueContextExt,
    resolve::{PrimaryResolveResult, ResolveResult, ResolveResultVc},
};
pub mod source_map;

pub use source_map::SourceMapReferenceVc;

/// A reference to one or multiple [Asset]s or other special things.
/// There are a bunch of optional traits that can influence how these references
/// are handled. e. g. [ChunkableAssetReference]
///
/// [Asset]: crate::asset::Asset
/// [ChunkableAssetReference]: crate::chunk::ChunkableAssetReference
#[turbo_tasks::value_trait]
pub trait AssetReference: ValueToString {
    fn resolve_reference(&self) -> ResolveResultVc;
    // TODO think about different types
    // fn kind(&self) -> AssetReferenceTypeVc;
}

/// Multiple [AssetReference]s
#[turbo_tasks::value(transparent)]
pub struct AssetReferences(Vec<AssetReferenceVc>);

#[turbo_tasks::value_impl]
impl AssetReferencesVc {
    /// An empty list of [AssetReference]s
    #[turbo_tasks::function]
    pub fn empty() -> Self {
        AssetReferencesVc::cell(Vec::new())
    }
}

/// A reference that always resolves to a single asset.
#[turbo_tasks::value]
pub struct SingleAssetReference {
    asset: AssetVc,
    description: StringVc,
}

impl SingleAssetReference {
    /// Returns the asset that this reference resolves to.
    pub fn asset(&self) -> AssetVc {
        self.asset
    }
}

#[turbo_tasks::value_impl]
impl AssetReference for SingleAssetReference {
    #[turbo_tasks::function]
    fn resolve_reference(&self) -> ResolveResultVc {
        ResolveResult::asset(self.asset).cell()
    }
}

#[turbo_tasks::value_impl]
impl ValueToString for SingleAssetReference {
    #[turbo_tasks::function]
    fn to_string(&self) -> StringVc {
        self.description
    }
}

#[turbo_tasks::value_impl]
impl SingleAssetReferenceVc {
    /// Create a new [SingleAssetReferenceVc] that resolves to the given asset.
    #[turbo_tasks::function]
    pub fn new(asset: AssetVc, description: StringVc) -> Self {
        Self::cell(SingleAssetReference { asset, description })
    }

    /// The [AssetVc] that this reference resolves to.
    #[turbo_tasks::function]
    pub async fn asset(self) -> Result<AssetVc> {
        Ok(self.await?.asset)
    }
}

/// Aggregates all [Asset]s referenced by an [Asset]. [AssetReference]
/// This does not include transitively references [Asset]s, but it includes
/// primary and secondary [Asset]s referenced.
///
/// [Asset]: crate::asset::Asset
#[turbo_tasks::function]
pub async fn all_referenced_assets(asset: AssetVc) -> Result<AssetsVc> {
    let references_set = asset.references().await?;
    let mut assets = Vec::new();
    let mut queue = VecDeque::with_capacity(32);
    for reference in references_set.iter() {
        queue.push_back(reference.resolve_reference());
    }
    // that would be non-deterministic:
    // while let Some(result) = race_pop(&mut queue).await {
    // match &*result? {
    while let Some(resolve_result) = queue.pop_front() {
        let ResolveResult {
            primary,
            references,
        } = &*resolve_result.await?;
        for result in primary {
            if let PrimaryResolveResult::Asset(asset) = *result {
                assets.push(asset);
            }
        }
        for reference in references {
            queue.push_back(reference.resolve_reference());
        }
    }
    Ok(AssetsVc::cell(assets))
}

/// Aggregates all primary [Asset]s referenced by an [Asset]. [AssetReference]
/// This does not include transitively references [Asset]s, only includes
/// primary [Asset]s referenced.
///
/// [Asset]: crate::asset::Asset
#[turbo_tasks::function]
pub async fn primary_referenced_assets(asset: AssetVc) -> Result<AssetsVc> {
    let assets = asset
        .references()
        .await?
        .iter()
        .map(|reference| async {
            let ResolveResult { primary, .. } = &*reference.resolve_reference().await?;
            Ok(primary
                .iter()
                .filter_map(|result| {
                    if let PrimaryResolveResult::Asset(asset) = *result {
                        Some(asset)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>())
        })
        .try_join()
        .await?
        .into_iter()
        .flatten()
        .collect();
    Ok(AssetsVc::cell(assets))
}

/// Aggregates all [Asset]s referenced by an [Asset] including transitively
/// referenced [Asset]s. This basically gives all [Asset]s in a subgraph
/// starting from the passed [Asset].
#[turbo_tasks::function]
pub async fn all_assets(asset: AssetVc) -> Result<AssetsVc> {
    // TODO need to track import path here
    let mut queue = VecDeque::with_capacity(32);
    queue.push_back((asset, all_referenced_assets(asset)));
    let mut assets = HashSet::new();
    assets.insert(asset);
    while let Some((parent, references)) = queue.pop_front() {
        let references = references
            .issue_context(parent.ident().path(), "expanding references of asset")
            .await?;
        for asset in references.await?.iter() {
            if assets.insert(*asset) {
                queue.push_back((*asset, all_referenced_assets(*asset)));
            }
        }
    }
    Ok(AssetsVc::cell(assets.into_iter().collect()))
}

/// Walks the asset graph from a single asset and collect all referenced assets.
#[turbo_tasks::function]
pub async fn all_assets_from_entry(entry: AssetVc) -> Result<AssetsVc> {
    Ok(AssetsVc::cell(
        AdjacencyMap::new()
            .skip_duplicates()
            .visit([entry], get_referenced_assets)
            .await
            .completed()?
            .into_inner()
            .into_reverse_topological()
            .collect(),
    ))
}

/// Computes the list of all chunk children of a given chunk.
async fn get_referenced_assets(asset: AssetVc) -> Result<impl Iterator<Item = AssetVc> + Send> {
    Ok(asset
        .references()
        .await?
        .iter()
        .map(|reference| async move {
            let primary_assets = reference.resolve_reference().primary_assets().await?;
            Ok(primary_assets.clone_value())
        })
        .try_join()
        .await?
        .into_iter()
        .flatten())
}
