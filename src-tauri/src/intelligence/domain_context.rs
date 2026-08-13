use serde::{Deserialize, Serialize};

const CORE: &str = include_str!("../../resources/intelligence/core.md");
const DOMAIN_INDEX: &str = include_str!("../../resources/intelligence/domain-index.md");
const ORDERS: &str = include_str!("../../resources/intelligence/orders.md");
const PRODUCTS: &str = include_str!("../../resources/intelligence/products.md");
const CUSTOMERS: &str = include_str!("../../resources/intelligence/customers.md");
const MONEY: &str = include_str!("../../resources/intelligence/money.md");
const FULFILLMENT: &str = include_str!("../../resources/intelligence/fulfillment.md");
const FORECASTING: &str = include_str!("../../resources/intelligence/forecasting.md");
const SAFETY: &str = include_str!("../../resources/intelligence/safety.md");

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum IntelligenceDomain {
    Orders,
    Products,
    Customers,
    Money,
    Fulfillment,
    Forecasting,
}

impl IntelligenceDomain {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Orders => "orders",
            Self::Products => "products",
            Self::Customers => "customers",
            Self::Money => "money",
            Self::Fulfillment => "fulfillment",
            Self::Forecasting => "forecasting",
        }
    }

    const fn document(self) -> &'static str {
        match self {
            Self::Orders => ORDERS,
            Self::Products => PRODUCTS,
            Self::Customers => CUSTOMERS,
            Self::Money => MONEY,
            Self::Fulfillment => FULFILLMENT,
            Self::Forecasting => FORECASTING,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DomainContextPack {
    pub(crate) domains: Vec<IntelligenceDomain>,
    pub(crate) documents: Vec<ContextDocument>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ContextDocument {
    pub(crate) name: &'static str,
    pub(crate) content: &'static str,
}

impl DomainContextPack {
    pub(crate) fn fingerprints(&self) -> Vec<(&'static str, String)> {
        self.documents
            .iter()
            .map(|document| (document.name, stable_fingerprint(document.content)))
            .collect()
    }
}

pub(crate) fn select_domain_context(question: &str) -> DomainContextPack {
    let normalized = question.to_lowercase();
    let mut domains = Vec::new();
    let forecasting = contains_any(
        &normalized,
        &[
            "forecast",
            "predict",
            "projection",
            "expected orders",
            "busy day",
        ],
    );
    let reorder = contains_any(
        &normalized,
        &[
            "reorder",
            "due to order again",
            "expected order date",
            "overdue order",
        ],
    );
    let products = contains_any(
        &normalized,
        &["product", "variant", "sku", "item", "selling", "sold"],
    );
    let customers = contains_any(
        &normalized,
        &[
            "customer", "buyer", "cohort", "inactive", "lapsed", "repeat",
        ],
    );
    let money = contains_any(
        &normalized,
        &[
            "order value",
            "average order value",
            "aov",
            "revenue",
            "monetary",
            "₹",
            "rs.",
        ],
    );
    let fulfillment = contains_any(
        &normalized,
        &[
            "fulfill",
            "delivery",
            "delivered",
            "packed",
            "packing",
            "shipped",
            "shipping",
        ],
    );

    domains.push(IntelligenceDomain::Orders);
    if products || reorder {
        domains.push(IntelligenceDomain::Products);
    }
    if customers || reorder {
        domains.push(IntelligenceDomain::Customers);
    }
    if money {
        domains.push(IntelligenceDomain::Money);
    }
    if fulfillment {
        domains.push(IntelligenceDomain::Fulfillment);
    }
    if forecasting || reorder {
        domains.push(IntelligenceDomain::Forecasting);
    }

    let mut documents = vec![
        ContextDocument {
            name: "core.md",
            content: CORE,
        },
        ContextDocument {
            name: "safety.md",
            content: SAFETY,
        },
    ];
    documents.extend(domains.iter().map(|domain| ContextDocument {
        name: match domain {
            IntelligenceDomain::Orders => "orders.md",
            IntelligenceDomain::Products => "products.md",
            IntelligenceDomain::Customers => "customers.md",
            IntelligenceDomain::Money => "money.md",
            IntelligenceDomain::Fulfillment => "fulfillment.md",
            IntelligenceDomain::Forecasting => "forecasting.md",
        },
        content: domain.document(),
    }));
    DomainContextPack { domains, documents }
}

fn stable_fingerprint(content: &str) -> String {
    let hash = content
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    format!("fnv1a64:{hash:016x}")
}

pub(crate) const fn domain_index() -> &'static str {
    DOMAIN_INDEX
}

fn contains_any(value: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| value.contains(term))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_only_broad_relevant_contexts() {
        let orders = select_domain_context("Compare orders month by month Jan through June");
        assert_eq!(orders.domains, vec![IntelligenceDomain::Orders]);

        let products = select_domain_context("Show top products May through June");
        assert_eq!(
            products.domains,
            vec![IntelligenceDomain::Orders, IntelligenceDomain::Products]
        );

        let customers =
            select_domain_context("Customers with AOV above ₹2,000 who haven't ordered recently");
        assert_eq!(
            customers.domains,
            vec![
                IntelligenceDomain::Orders,
                IntelligenceDomain::Customers,
                IntelligenceDomain::Money
            ]
        );

        let forecast = select_domain_context("Forecast next month's daily order volume");
        assert_eq!(
            forecast.domains,
            vec![IntelligenceDomain::Orders, IntelligenceDomain::Forecasting]
        );
        assert!(domain_index().contains("FORECASTING"));

        let reorder = select_domain_context("Which customers are due to reorder soon?");
        assert_eq!(
            reorder.domains,
            vec![
                IntelligenceDomain::Orders,
                IntelligenceDomain::Products,
                IntelligenceDomain::Customers,
                IntelligenceDomain::Forecasting
            ]
        );
    }

    #[test]
    fn context_files_do_not_expose_denied_customer_fields() {
        let context = select_domain_context("Which customers ordered most?");
        let combined = context
            .documents
            .iter()
            .map(|document| document.content)
            .collect::<String>();
        assert!(!combined.contains("password_hash"));
        assert!(!combined.contains("DATABASE_URL"));
    }
}
