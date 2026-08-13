mod context;
mod domain_context;
mod registry;
mod types;

pub(crate) use context::context_for_question;
pub(crate) use domain_context::{
    domain_index, select_domain_context, DomainContextPack, IntelligenceDomain,
};
