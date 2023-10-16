use anyhow::Result;
use lightningcss::{
    media_query::MediaList,
    printer::Printer,
    properties::custom::TokenList,
    rules::{
        import::ImportRule,
        layer::{LayerName, LayerStatementRule},
        media::MediaRule,
        supports::{SupportsCondition, SupportsRule},
        unknown::UnknownAtRule,
        CssRule, CssRuleList, Location,
    },
    stylesheet::PrinterOptions,
    traits::ToCss,
};
use turbo_tasks::{Value, ValueToString, Vc};
use turbopack_core::{
    chunk::{ChunkableModuleReference, ChunkingContext},
    issue::IssueSource,
    reference::ModuleReference,
    reference_type::CssReferenceSubType,
    resolve::{origin::ResolveOrigin, parse::Request, ModuleResolveResult},
};

use crate::{
    chunk::CssImport,
    code_gen::{CodeGenerateable, CodeGeneration},
    references::css_resolve,
};

#[turbo_tasks::value(into = "new", eq = "manual", serialization = "none")]
pub struct ImportAttributes {
    #[turbo_tasks(trace_ignore)]
    pub layer_name: Option<LayerName<'static>>,
    #[turbo_tasks(trace_ignore)]
    pub supports: Option<SupportsCondition<'static>>,
    #[turbo_tasks(trace_ignore)]
    pub media: MediaList<'static>,
}

impl PartialEq for ImportAttributes {
    fn eq(&self, _: &Self) -> bool {
        false
    }
}

impl ImportAttributes {
    pub fn new_from_prelude(prelude: &ImportRule<'static>) -> Self {
        let layer_name = prelude.layer.clone().flatten();

        let supports = prelude.supports.clone();

        let media = prelude.media.clone();

        Self {
            layer_name,
            supports,
            media,
        }
    }

    pub fn print_block(&self) -> Result<(String, String)> {
        // something random that's never gonna be in real css
        // Box::new(ListOfComponentValues {
        //     span: DUMMY_SP,
        //     children: vec![ComponentValue::PreservedToken(Box::new(token(
        //         Token::String {
        //             value: Default::default(),
        //             raw: r#""""__turbopack_placeholder__""""#.into(),
        //         },
        //     )))],
        // })

        let default_loc = Location {
            source_index: 0,
            line: 0,
            column: 0,
        };

        let mut rule: CssRule = CssRule::Unknown(UnknownAtRule {
            name: r#""""__turbopack_placeholder__""""#.into(),
            prelude: TokenList(vec![]),
            block: None,
            loc: default_loc,
        });

        if !self.media.media_queries.is_empty() {
            rule = CssRule::Media(MediaRule {
                query: self.media.clone(),
                rules: CssRuleList(vec![rule]),
                loc: default_loc,
            })
        }

        if let Some(supports) = &self.supports {
            rule = CssRule::Supports(SupportsRule {
                condition: supports.clone(),
                rules: CssRuleList(vec![rule]),
                loc: default_loc,
            })
        }
        if let Some(layer_name) = &self.layer_name {
            rule = CssRule::LayerStatement(LayerStatementRule {
                names: vec![layer_name.clone()],
                loc: default_loc,
            });
        }

        let mut output = String::new();
        let mut printer = Printer::new(&mut output, PrinterOptions::default());
        rule.to_css(&mut printer)?;

        let (open, close) = output
            .split_once(r#""""__turbopack_placeholder__""""#)
            .unwrap();

        Ok((open.trim().into(), close.trim().into()))
    }
}

#[turbo_tasks::value]
#[derive(Hash, Debug)]
pub struct ImportAssetReference {
    pub origin: Vc<Box<dyn ResolveOrigin>>,
    pub request: Vc<Request>,
    pub attributes: Vc<ImportAttributes>,
    pub issue_source: Vc<IssueSource>,
}

#[turbo_tasks::value_impl]
impl ImportAssetReference {
    #[turbo_tasks::function]
    pub fn new(
        origin: Vc<Box<dyn ResolveOrigin>>,
        request: Vc<Request>,
        attributes: Vc<ImportAttributes>,
        issue_source: Vc<IssueSource>,
    ) -> Vc<Self> {
        Self::cell(ImportAssetReference {
            origin,
            request,
            attributes,
            issue_source,
        })
    }
}

#[turbo_tasks::value_impl]
impl ModuleReference for ImportAssetReference {
    #[turbo_tasks::function]
    fn resolve_reference(&self) -> Vc<ModuleResolveResult> {
        css_resolve(
            self.origin,
            self.request,
            Value::new(CssReferenceSubType::AtImport),
            Some(self.issue_source),
        )
    }
}

#[turbo_tasks::value_impl]
impl ValueToString for ImportAssetReference {
    #[turbo_tasks::function]
    async fn to_string(&self) -> Result<Vc<String>> {
        Ok(Vc::cell(format!(
            "import(url) {}",
            self.request.to_string().await?,
        )))
    }
}

#[turbo_tasks::value_impl]
impl CodeGenerateable for ImportAssetReference {
    #[turbo_tasks::function]
    async fn code_generation(
        self: Vc<Self>,
        _context: Vc<Box<dyn ChunkingContext>>,
    ) -> Result<Vc<CodeGeneration>> {
        let this = &*self.await?;
        let mut imports = vec![];
        if let Request::Uri {
            protocol,
            remainder,
        } = &*this.request.await?
        {
            imports.push(CssImport::External(Vc::cell(format!(
                "{}{}",
                protocol, remainder
            ))))
        }

        Ok(CodeGeneration { imports }.into())
    }
}

#[turbo_tasks::value_impl]
impl ChunkableModuleReference for ImportAssetReference {}
