use crate::{
    agent::AgentError,
    database::{validate_sql, Database},
};

const MIN_PRODUCT_MATCH_SCORE: f64 = 0.72;
const AMBIGUITY_GAP: f64 = 0.03;
const MAX_AMBIGUOUS_CHOICES: usize = 5;

const PRODUCT_CATALOGUE_SQL: &str = r#"
SELECT
    p.id AS product_id,
    p.title AS product_name
FROM product AS p
WHERE p.deleted_at IS NULL
  AND lower(COALESCE(p.status::text, '')) = 'published'
  AND p.title IS NOT NULL
ORDER BY p.title
LIMIT 1000
"#;

#[derive(Clone, Debug)]
pub(crate) struct ResolvedProduct {
    pub(crate) product_id: String,
    pub(crate) product_name: String,
    pub(crate) score: f64,
}

#[derive(Clone, Debug)]
pub(crate) enum ProductResolution {
    Resolved(ResolvedProduct),
    Ambiguous(Vec<ResolvedProduct>),
    NotFound,
}

#[derive(Clone, Debug)]
pub(crate) struct ProductResolutionLookup {
    pub(crate) resolution: ProductResolution,
    pub(crate) db_duration_ms: u128,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedProductMention {
    pub(crate) mention: String,
    pub(crate) product: ResolvedProduct,
}

#[derive(Clone, Debug)]
pub(crate) enum MultiProductResolution {
    Resolved(Vec<ResolvedProductMention>),

    Ambiguous {
        mention: String,
        candidates: Vec<ResolvedProduct>,
    },

    NotFound {
        mention: String,
    },

    DuplicateResolvedProduct {
        first_mention: String,
        duplicate_mention: String,
        product: ResolvedProduct,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct MultiProductResolutionLookup {
    pub(crate) resolution: MultiProductResolution,
    pub(crate) db_duration_ms: u128,
}

#[derive(Clone, Debug)]
struct CatalogueProduct {
    product_id: String,
    product_name: String,
}

pub(crate) async fn resolve_product(
    database: &Database,
    mention: &str,
) -> Result<ProductResolutionLookup, AgentError> {
    let (catalogue, db_duration_ms) = load_product_catalogue(database).await?;

    Ok(ProductResolutionLookup {
        resolution: resolve_product_from_catalogue(mention, &catalogue),
        db_duration_ms,
    })
}

pub(crate) async fn resolve_products(
    database: &Database,
    mentions: &[String],
) -> Result<MultiProductResolutionLookup, AgentError> {
    let (catalogue, db_duration_ms) = load_product_catalogue(database).await?;

    Ok(MultiProductResolutionLookup {
        resolution: resolve_products_from_catalogue(mentions, &catalogue),
        db_duration_ms,
    })
}

async fn load_product_catalogue(
    database: &Database,
) -> Result<(Vec<CatalogueProduct>, u128), AgentError> {
    let validated = validate_sql(PRODUCT_CATALOGUE_SQL)?;

    let result = database.execute_deterministic(&validated, &[]).await?;

    let db_duration_ms = result.duration_ms;

    let catalogue = result
        .rows
        .iter()
        .filter_map(|row| {
            let product_id = row.get("product_id")?.as_str()?.trim();

            let product_name = row.get("product_name")?.as_str()?.trim();

            if product_id.is_empty() || product_name.is_empty() {
                return None;
            }

            Some(CatalogueProduct {
                product_id: product_id.to_owned(),
                product_name: product_name.to_owned(),
            })
        })
        .collect::<Vec<_>>();

    Ok((catalogue, db_duration_ms))
}

fn resolve_products_from_catalogue(
    mentions: &[String],
    catalogue: &[CatalogueProduct],
) -> MultiProductResolution {
    let mut resolved = Vec::<ResolvedProductMention>::with_capacity(mentions.len());

    for mention in mentions {
        match resolve_product_from_catalogue(mention, catalogue) {
            ProductResolution::Resolved(product) => {
                /*
                 * Two different user mentions can still resolve to the
                 * same canonical product.
                 *
                 * Example:
                 * "Sharbati Atta"
                 * "MP Sharbati Atta"
                 *
                 * B5 comparisons must contain distinct canonical
                 * products, so do not silently compare the same product
                 * twice.
                 */
                if let Some(existing) = resolved
                    .iter()
                    .find(|existing| existing.product.product_id == product.product_id)
                {
                    return MultiProductResolution::DuplicateResolvedProduct {
                        first_mention: existing.mention.clone(),
                        duplicate_mention: mention.clone(),
                        product,
                    };
                }

                resolved.push(ResolvedProductMention {
                    mention: mention.clone(),
                    product,
                });
            }

            ProductResolution::Ambiguous(candidates) => {
                return MultiProductResolution::Ambiguous {
                    mention: mention.clone(),
                    candidates,
                };
            }

            ProductResolution::NotFound => {
                return MultiProductResolution::NotFound {
                    mention: mention.clone(),
                };
            }
        }
    }

    MultiProductResolution::Resolved(resolved)
}

fn resolve_product_from_catalogue(
    mention: &str,
    catalogue: &[CatalogueProduct],
) -> ProductResolution {
    let normalized_mention = normalize_text(mention);

    if normalized_mention.is_empty() {
        return ProductResolution::NotFound;
    }

    let mut candidates = catalogue
        .iter()
        .map(|product| ResolvedProduct {
            product_id: product.product_id.clone(),
            product_name: product.product_name.clone(),
            score: product_match_score(&normalized_mention, &product.product_name),
        })
        .filter(|candidate| candidate.score >= MIN_PRODUCT_MATCH_SCORE)
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.product_name.cmp(&right.product_name))
            .then_with(|| left.product_id.cmp(&right.product_id))
    });

    let Some(best) = candidates.first().cloned() else {
        return ProductResolution::NotFound;
    };

    /*
     * Exact normalized matches and exact matches after dropping a short
     * catalogue prefix such as "MP" are authoritative.
     */
    if best.score >= 0.99 {
        return ProductResolution::Resolved(best);
    }

    /*
     * A single exact catalogue token is too weak to choose between
     * multiple products that share that token.
     */
    let mention_tokens = normalized_mention.split_whitespace().collect::<Vec<_>>();

    if mention_tokens.len() == 1 {
        let mention_token = mention_tokens[0];

        let token_matches = candidates
            .iter()
            .filter(|candidate| {
                normalize_text(&candidate.product_name)
                    .split_whitespace()
                    .any(|product_token| product_token == mention_token)
            })
            .take(MAX_AMBIGUOUS_CHOICES)
            .cloned()
            .collect::<Vec<_>>();

        if token_matches.len() > 1 {
            return ProductResolution::Ambiguous(token_matches);
        }
    }

    let ambiguous = candidates
        .iter()
        .take_while(|candidate| best.score - candidate.score <= AMBIGUITY_GAP)
        .take(MAX_AMBIGUOUS_CHOICES)
        .cloned()
        .collect::<Vec<_>>();

    if ambiguous.len() > 1 {
        return ProductResolution::Ambiguous(ambiguous);
    }

    ProductResolution::Resolved(best)
}

fn product_match_score(normalized_mention: &str, product_name: &str) -> f64 {
    let normalized_product = normalize_text(product_name);

    if normalized_product.is_empty() {
        return 0.0;
    }

    if normalized_mention == normalized_product {
        return 1.0;
    }

    let stripped_product = strip_short_leading_token(&normalized_product);

    /*
     * Handles:
     *
     * "sharbati atta"
     *     -> "MP Sharbati Atta"
     */
    if normalized_mention == stripped_product {
        return 0.995;
    }

    let mention_tokens = normalized_mention.split_whitespace().collect::<Vec<_>>();

    let product_tokens = normalized_product.split_whitespace().collect::<Vec<_>>();

    if mention_tokens.is_empty() || product_tokens.is_empty() {
        return 0.0;
    }

    let extra_tokens = product_tokens.len().saturating_sub(mention_tokens.len()) as f64;

    /*
     * Strong contiguous phrase match.
     *
     * "sharbati atta" strongly prefers
     * "MP Sharbati Atta" over
     * "Sharbati Wheat Atta".
     */
    let phrase_score = if normalized_product.contains(normalized_mention)
        || stripped_product.contains(normalized_mention)
    {
        0.96 - (extra_tokens * 0.015).min(0.12)
    } else {
        0.0
    };

    /*
     * Whole-string similarity handles small spelling errors:
     *
     * "sharbti atta"
     *     -> "sharbati atta"
     */
    let whole_string_score = string_similarity(normalized_mention, stripped_product)
        .max(string_similarity(normalized_mention, &normalized_product))
        * 0.97;

    /*
     * Token similarity is a fallback for word-level misspellings
     * and small wording differences.
     */
    let token_similarity_sum = mention_tokens
        .iter()
        .map(|mention_token| {
            product_tokens
                .iter()
                .map(|product_token| string_similarity(mention_token, product_token))
                .fold(0.0_f64, f64::max)
        })
        .sum::<f64>();

    let token_average = token_similarity_sum / mention_tokens.len() as f64;

    let token_score = token_average * 0.90 - (extra_tokens * 0.02).min(0.15);

    phrase_score
        .max(whole_string_score)
        .max(token_score)
        .clamp(0.0, 1.0)
}

fn normalize_text(value: &str) -> String {
    value
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_short_leading_token(value: &str) -> &str {
    let Some((first, remainder)) = value.split_once(' ') else {
        return value;
    };

    if first.chars().count() <= 2 && !remainder.trim().is_empty() {
        remainder
    } else {
        value
    }
}

fn string_similarity(left: &str, right: &str) -> f64 {
    if left == right {
        return 1.0;
    }

    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();

    let maximum_length = left_chars.len().max(right_chars.len());

    if maximum_length == 0 {
        return 1.0;
    }

    let distance = levenshtein_distance(&left_chars, &right_chars);

    1.0 - distance as f64 / maximum_length as f64
}

fn levenshtein_distance(left: &[char], right: &[char]) -> usize {
    if left.is_empty() {
        return right.len();
    }

    if right.is_empty() {
        return left.len();
    }

    let mut previous = (0..=right.len()).collect::<Vec<_>>();

    let mut current = vec![0; right.len() + 1];

    for (left_index, left_character) in left.iter().enumerate() {
        current[0] = left_index + 1;

        for (right_index, right_character) in right.iter().enumerate() {
            let substitution_cost = usize::from(left_character != right_character);

            current[right_index + 1] = (current[right_index] + 1)
                .min(previous[right_index + 1] + 1)
                .min(previous[right_index] + substitution_cost);
        }

        std::mem::swap(&mut previous, &mut current);
    }

    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue_product(product_id: &str, product_name: &str) -> CatalogueProduct {
        CatalogueProduct {
            product_id: product_id.to_owned(),
            product_name: product_name.to_owned(),
        }
    }

    #[test]
    fn exact_name_is_strongest_match() {
        assert_eq!(
            product_match_score("mp sharbati atta", "MP Sharbati Atta",),
            1.0
        );
    }

    #[test]
    fn omitted_short_prefix_still_resolves_strongly() {
        assert!(product_match_score("sharbati atta", "MP Sharbati Atta",) >= 0.99);
    }

    #[test]
    fn contiguous_name_beats_words_separated_by_extra_terms() {
        let direct = product_match_score("sharbati atta", "MP Sharbati Atta");

        let separated = product_match_score("sharbati atta", "Sharbati Wheat Atta");

        assert!(direct > separated);
    }

    #[test]
    fn minor_typo_remains_resolvable() {
        assert!(
            product_match_score("sharbti atta", "MP Sharbati Atta",) >= MIN_PRODUCT_MATCH_SCORE
        );
    }

    #[test]
    fn unrelated_product_is_rejected() {
        assert!(
            product_match_score("sharbati atta", "Khapli Multigrain Atta",)
                < MIN_PRODUCT_MATCH_SCORE
        );
    }
    #[test]
    fn single_shared_product_token_is_ambiguous() {
        let mention = normalize_text("sharbati");

        let candidates = ["Sharbati Daliya", "MP Sharbati Atta"]
            .into_iter()
            .map(|product_name| ResolvedProduct {
                product_id: product_name.to_owned(),
                product_name: product_name.to_owned(),
                score: product_match_score(&mention, product_name),
            })
            .filter(|candidate| candidate.score >= MIN_PRODUCT_MATCH_SCORE)
            .collect::<Vec<_>>();

        let matching = candidates
            .iter()
            .filter(|candidate| {
                normalize_text(&candidate.product_name)
                    .split_whitespace()
                    .any(|token| token == "sharbati")
            })
            .collect::<Vec<_>>();

        assert_eq!(matching.len(), 2);
    }
    #[test]
    fn multi_product_resolution_resolves_every_requested_product_in_order() {
        let catalogue = vec![
            catalogue_product("prod_sharbati", "MP Sharbati Atta"),
            catalogue_product("prod_khapli", "Khapli/Emmer Wheat Atta"),
            catalogue_product("prod_multigrain", "Multigrain Atta"),
        ];

        let mentions = vec![
            "khapli emmer".to_owned(),
            "sharbati atta".to_owned(),
            "multigrain atta".to_owned(),
        ];

        let resolution = resolve_products_from_catalogue(&mentions, &catalogue);

        let MultiProductResolution::Resolved(resolved) = resolution else {
            panic!("expected all product mentions to resolve");
        };

        assert_eq!(resolved.len(), 3);

        assert_eq!(resolved[0].mention, "khapli emmer");
        assert_eq!(resolved[0].product.product_id, "prod_khapli");
        assert_eq!(resolved[0].product.product_name, "Khapli/Emmer Wheat Atta");

        assert_eq!(resolved[1].mention, "sharbati atta");
        assert_eq!(resolved[1].product.product_id, "prod_sharbati");

        assert_eq!(resolved[2].mention, "multigrain atta");
        assert_eq!(resolved[2].product.product_id, "prod_multigrain");
    }
    #[test]
    fn multi_product_resolution_aborts_when_one_mention_is_ambiguous() {
        let catalogue = vec![
            catalogue_product("prod_sharbati_atta", "MP Sharbati Atta"),
            catalogue_product("prod_sharbati_daliya", "Sharbati Daliya"),
            catalogue_product("prod_khapli", "Khapli/Emmer Wheat Atta"),
        ];

        let mentions = vec!["khapli emmer".to_owned(), "sharbati".to_owned()];

        let resolution = resolve_products_from_catalogue(&mentions, &catalogue);

        let MultiProductResolution::Ambiguous {
            mention,
            candidates,
        } = resolution
        else {
            panic!("expected ambiguous product mention");
        };

        assert_eq!(mention, "sharbati");

        assert_eq!(candidates.len(), 2);

        assert!(candidates
            .iter()
            .any(|candidate| { candidate.product_id == "prod_sharbati_atta" }));

        assert!(candidates
            .iter()
            .any(|candidate| { candidate.product_id == "prod_sharbati_daliya" }));
    }
    #[test]
    fn multi_product_resolution_aborts_when_one_mention_is_not_found() {
        let catalogue = vec![
            catalogue_product("prod_sharbati", "MP Sharbati Atta"),
            catalogue_product("prod_khapli", "Khapli/Emmer Wheat Atta"),
        ];

        let mentions = vec![
            "sharbati atta".to_owned(),
            "completely nonexistent product".to_owned(),
        ];

        let resolution = resolve_products_from_catalogue(&mentions, &catalogue);

        let MultiProductResolution::NotFound { mention } = resolution else {
            panic!("expected unresolved product mention");
        };

        assert_eq!(mention, "completely nonexistent product");
    }
    #[test]
    fn multi_product_resolution_rejects_two_mentions_of_same_canonical_product() {
        let catalogue = vec![
            catalogue_product("prod_sharbati", "MP Sharbati Atta"),
            catalogue_product("prod_khapli", "Khapli/Emmer Wheat Atta"),
        ];

        let mentions = vec!["MP Sharbati Atta".to_owned(), "Sharbati Atta".to_owned()];

        let resolution = resolve_products_from_catalogue(&mentions, &catalogue);

        let MultiProductResolution::DuplicateResolvedProduct {
            first_mention,
            duplicate_mention,
            product,
        } = resolution
        else {
            panic!("expected duplicate canonical product");
        };

        assert_eq!(first_mention, "MP Sharbati Atta");

        assert_eq!(duplicate_mention, "Sharbati Atta");

        assert_eq!(product.product_id, "prod_sharbati");
    }
    #[test]
    fn multi_product_resolution_uses_existing_typo_tolerance() {
        let catalogue = vec![
            catalogue_product("prod_sharbati", "MP Sharbati Atta"),
            catalogue_product("prod_khapli", "Khapli/Emmer Wheat Atta"),
        ];

        let mentions = vec!["sharbti atta".to_owned(), "khapli emmer".to_owned()];

        let resolution = resolve_products_from_catalogue(&mentions, &catalogue);

        let MultiProductResolution::Resolved(resolved) = resolution else {
            panic!("expected typo-tolerant resolution");
        };

        assert_eq!(resolved.len(), 2);

        assert_eq!(resolved[0].product.product_id, "prod_sharbati");

        assert_eq!(resolved[1].product.product_id, "prod_khapli");
    }
}
