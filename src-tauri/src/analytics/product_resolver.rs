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

pub(crate) async fn resolve_product(
    database: &Database,
    mention: &str,
) -> Result<ProductResolutionLookup, AgentError> {
    let normalized_mention = normalize_text(mention);

    if normalized_mention.is_empty() {
        return Ok(ProductResolutionLookup {
            resolution: ProductResolution::NotFound,
            db_duration_ms: 0,
        });
    }

    let validated = validate_sql(PRODUCT_CATALOGUE_SQL)?;

    let result = database.execute_deterministic(&validated, &[]).await?;

    let db_duration_ms = result.duration_ms;

    let mut candidates = result
        .rows
        .iter()
        .filter_map(|row| {
            let product_id = row.get("product_id")?.as_str()?.trim();

            let product_name = row.get("product_name")?.as_str()?.trim();

            if product_id.is_empty() || product_name.is_empty() {
                return None;
            }

            Some(ResolvedProduct {
                product_id: product_id.to_owned(),
                product_name: product_name.to_owned(),
                score: product_match_score(&normalized_mention, product_name),
            })
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
        return Ok(ProductResolutionLookup {
            resolution: ProductResolution::NotFound,
            db_duration_ms,
        });
    };

    /*
     * Exact normalized matches and exact matches after dropping a short
     * catalogue prefix such as "MP" are authoritative.
     */
    if best.score >= 0.99 {
        return Ok(ProductResolutionLookup {
            resolution: ProductResolution::Resolved(best),
            db_duration_ms,
        });
    }

    /*
     * A single exact catalogue token is too weak to choose between
     * multiple products that share that token.
     *
     * Example:
     *
     * "sharbati"
     *
     * may match:
     * - MP Sharbati Atta
     * - Sharbati Daliya
     *
     * In that case asking for clarification is safer than selecting
     * whichever title happens to be shorter.
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
            return Ok(ProductResolutionLookup {
                resolution: ProductResolution::Ambiguous(token_matches),
                db_duration_ms,
            });
        }
    }

    let ambiguous = candidates
        .iter()
        .take_while(|candidate| best.score - candidate.score <= AMBIGUITY_GAP)
        .take(MAX_AMBIGUOUS_CHOICES)
        .cloned()
        .collect::<Vec<_>>();

    if ambiguous.len() > 1 {
        return Ok(ProductResolutionLookup {
            resolution: ProductResolution::Ambiguous(ambiguous),
            db_duration_ms,
        });
    }

    Ok(ProductResolutionLookup {
        resolution: ProductResolution::Resolved(best),
        db_duration_ms,
    })
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
}
