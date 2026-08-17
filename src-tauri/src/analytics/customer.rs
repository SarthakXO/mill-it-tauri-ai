use super::customer_plan::{
    CustomerAnalysis, CustomerAnalysisPlan, CustomerProgressionSubject, CustomerSegment,
    LOYAL_CUSTOMER_LOOKBACK_MONTHS, LOYAL_CUSTOMER_MIN_ORDERS,
};
use crate::{
    agent::{AgentError, AgentResponse},
    database::{validate_sql, Database, QueryParameter, QueryResult},
};
use chrono::Months;
use serde_json::Value;
use std::time::Instant;

pub(crate) struct CustomerAnalysisExecution {
    pub(crate) response: AgentResponse,
    pub(crate) rows: usize,
    pub(crate) db_duration_ms: u128,
    pub(crate) calculation_duration_ms: u128,
    pub(crate) summary_duration_ms: u128,
}

struct CompiledCustomerAnalysis {
    sql: String,
    parameters: Vec<QueryParameter>,
    expected_shape: &'static str,
    eligibility_start: chrono::NaiveDate,
}

pub(crate) async fn execute_customer_analysis_plan(
    database: &Database,
    question: &str,
    request_id: &str,
    plan: &CustomerAnalysisPlan,
) -> Result<CustomerAnalysisExecution, AgentError> {
    plan.validate()
        .map_err(|error| AgentError::InvalidPlan(error.to_string()))?;

    let compiled = compile_customer_analysis(plan).map_err(AgentError::InvalidPlan)?;

    #[cfg(debug_assertions)]
    {
        eprintln!(
            "[customer analysis plan]\nrequest_id={}\nsegment={:?}\nanalysis={:?}\nperiod={}..{}\neligibility_window={}..{}",
            request_id,
            plan.segment,
            plan.analysis,
            plan.period.start,
            plan.period.end_exclusive,
            compiled.eligibility_start,
            plan.period.end_exclusive,
        );

        eprintln!(
            "[customer analysis query]\nrequest_id={}\nsql={}\nparams={}",
            request_id,
            compiled.sql,
            safe_parameters_for_log(&compiled.parameters),
        );
    }

    /*
     * Defense in depth:
     *
     * Customer-analysis SQL is deterministic Rust-owned SQL, but it still
     * passes through the same read-only AST validator before PostgreSQL.
     */
    let validated = validate_sql(&compiled.sql)?;

    if validated.parameter_count() != compiled.parameters.len() {
        return Err(AgentError::InvalidPlan(
            "customer analysis query placeholder count did not match its parameters".to_owned(),
        ));
    }

    let result = database
        .execute_deterministic(&validated, &compiled.parameters)
        .await?;

    let db_duration_ms = result.duration_ms;

    /*
     * The first customer-analysis operation performs its percentages
     * directly in deterministic SQL, so there is no additional numerical
     * transformation required here yet.
     */
    let calculation_started = Instant::now();

    let calculation_duration_ms = calculation_started.elapsed().as_millis();

    let summary_started = Instant::now();

    let answer = summarize_customer_analysis(plan, &result);

    let summary_duration_ms = summary_started.elapsed().as_millis();

    let mut response = AgentResponse::from_query(
        answer,
        question,
        "customer purchase progression",
        compiled.expected_shape,
        &result,
        None,
        None,
    );

    if let Some(table) = response.tables.first_mut() {
        table.id = "customer-purchase-progression".to_owned();

        table.title = "Customer purchase progression".to_owned();
    }

    #[cfg(debug_assertions)]
    eprintln!(
        "[customer analysis summary]\nrequest_id={}\nrows={}\ndb_duration_ms={}\ncalculation_duration_ms={}\nsummary_duration_ms={}",
        request_id,
        result.row_count,
        db_duration_ms,
        calculation_duration_ms,
        summary_duration_ms,
    );

    Ok(CustomerAnalysisExecution {
        response,
        rows: result.row_count,
        db_duration_ms,
        calculation_duration_ms,
        summary_duration_ms,
    })
}

fn compile_customer_analysis(
    plan: &CustomerAnalysisPlan,
) -> Result<CompiledCustomerAnalysis, String> {
    plan.validate().map_err(|error| error.to_string())?;

    let max_purchase_number = match &plan.analysis {
        CustomerAnalysis::PurchaseProgression {
            subject: CustomerProgressionSubject::Category,
            max_purchase_number,
        } => *max_purchase_number,

        CustomerAnalysis::PurchaseProgression { .. } => {
            return Err(
                "product and variant purchase progression are not implemented yet".to_owned(),
            );
        }

        CustomerAnalysis::ProductReorder { .. } => {
            return Err("product reorder customer analysis is not implemented yet".to_owned());
        }

        CustomerAnalysis::FrequencyDistribution { .. } => {
            return Err("customer frequency distribution is not implemented yet".to_owned());
        }

        CustomerAnalysis::CohortRetention { .. } => {
            return Err("customer cohort retention is not implemented yet".to_owned());
        }
    };

    /*
     * First live customer-analysis capability is deliberately restricted
     * to the locked Mill-it loyal-customer definition.
     *
     * The typed contract already supports CustomerSegment::All so later
     * customer operations do not require a contract redesign.
     */
    if plan.segment != CustomerSegment::Loyal {
        return Err(
            "category purchase progression currently requires the loyal customer segment"
                .to_owned(),
        );
    }

    let eligibility_start = plan
        .period
        .end_exclusive
        .checked_sub_months(Months::new(LOYAL_CUSTOMER_LOOKBACK_MONTHS))
        .ok_or_else(|| "could not resolve the loyal-customer eligibility window".to_owned())?;

    let parameters = vec![
        /*
         * $1
         *
         * Authoritative historical as-of boundary. All lifetime
         * sequencing stops before this exclusive date.
         */
        QueryParameter::String(plan.period.end_exclusive.to_string()),
        /*
         * $2
         *
         * Loyalty eligibility starts exactly four months before the
         * analysis as-of boundary.
         */
        QueryParameter::String(eligibility_start.to_string()),
        /*
         * $3
         *
         * >= 3 qualifying orders in that four-month window.
         */
        QueryParameter::Integer(i64::from(LOYAL_CUSTOMER_MIN_ORDERS)),
        /*
         * $4
         *
         * Bound lifetime purchase sequencing to the requested ordinal.
         */
        QueryParameter::Integer(i64::from(max_purchase_number)),
    ];

    Ok(CompiledCustomerAnalysis {
        sql: CATEGORY_PURCHASE_PROGRESSION_SQL.to_owned(),
        parameters,
        expected_shape: "category_breakdown",
        eligibility_start,
    })
}

/*
 * Important analytical semantics:
 *
 * 1. Loyalty membership:
 *      >= 3 qualifying orders in trailing 4 months.
 *
 * 2. Purchase sequence:
 *      lifetime qualifying orders before the as-of boundary.
 *
 * 3. Purchase number:
 *      ROW_NUMBER() by customer, ordered by created_at then order_id.
 *
 * 4. Category:
 *      exactly one canonical broad business category per product.
 *      Products without a mapped category become "Unmapped".
 *
 * 5. Category incidence:
 *      one customer can contribute to several categories at the same
 *      purchase ordinal if that basket contains several categories.
 *
 * Therefore category_penetration_pct values for one purchase number
 * are NOT expected to sum to 100%.
 */
const CATEGORY_PURCHASE_PROGRESSION_SQL: &str = r#"
WITH RECURSIVE

eligible_customers AS (
    SELECT
        o.customer_id
    FROM "order" AS o
    WHERE
        o.customer_id IS NOT NULL
        AND timezone('Asia/Kolkata', o.created_at) >= (($2::text)::date)
        AND timezone('Asia/Kolkata', o.created_at) < (($1::text)::date)
        AND NOT (
            lower(o.status::text) IN ('canceled', 'cancelled')
            OR upper(COALESCE(o.zoho_order_status, ''))
                IN ('CANCELED', 'CANCELLED')
        )
    GROUP BY
        o.customer_id
    HAVING
        COUNT(DISTINCT o.id) >= ($3::bigint)
),

qualifying_history AS (
    SELECT
        o.id AS order_id,
        o.customer_id,
        o.created_at
    FROM "order" AS o
    JOIN eligible_customers AS ec
        ON ec.customer_id = o.customer_id
    WHERE
        o.customer_id IS NOT NULL
        AND timezone('Asia/Kolkata', o.created_at) < (($1::text)::date)
        AND NOT (
            lower(o.status::text) IN ('canceled', 'cancelled')
            OR upper(COALESCE(o.zoho_order_status, ''))
                IN ('CANCELED', 'CANCELLED')
        )
),

sequenced_orders AS (
    SELECT
        qh.order_id,
        qh.customer_id,
        ROW_NUMBER() OVER (
            PARTITION BY qh.customer_id
            ORDER BY qh.created_at, qh.order_id
        ) AS purchase_number
    FROM qualifying_history AS qh
),

product_category_tree AS (
    /*
     * Start with every category directly attached to the product.
     */
    SELECT
        pcp.product_id,
        pc.id AS category_id,
        pc.parent_category_id,
        ARRAY[pc.id] AS category_path
    FROM product_category_product AS pcp
    JOIN product_category AS pc
        ON pc.id = pcp.product_category_id

    UNION ALL

    /*
     * Walk upward until a broad mapped business category is reached.
     *
     * category_path prevents accidental catalogue cycles from creating
     * an unbounded recursive query.
     */
    SELECT
        pct.product_id,
        parent.id AS category_id,
        parent.parent_category_id,
        pct.category_path || parent.id
    FROM product_category_tree AS pct
    JOIN product_category AS parent
        ON parent.id = pct.parent_category_id
    WHERE
        NOT parent.id = ANY(pct.category_path)
),

canonical_product_category AS (
    /*
     * A product can belong to multiple catalogue categories.
     *
     * Select exactly one canonical broad business category per product
     * using the same deterministic priority order used by the existing
     * Mill-it catalogue classification logic.
     */
    SELECT DISTINCT ON (pct.product_id)
        pct.product_id,

        CASE pct.category_id
            WHEN 'pcat_01KRBDPYHB2C5CPV03929QQQFK'
                THEN 'Atta & Daliya'
            WHEN 'pcat_01KRCEBEX5NSMTW7FJZP9QN961'
                THEN 'Bakery'
            WHEN 'pcat_01KRCCWXM5B9H4N1SGPMKAWPGV'
                THEN 'Ghee & Oils'
            WHEN 'pcat_01KRCE52VH4P3831FKKE023DQX'
                THEN 'Pre-mixes'
            WHEN 'pcat_01KRCDZ157N2T2F2AZR5ZBJT4K'
                THEN 'Rice & Pulses'
            WHEN 'pcat_01KRCEDKVETGQF0405BQXB53X7'
                THEN 'Sweets & Snacks'
            WHEN 'pcat_01KRCE2TNZWTD5G04VTD5AA749'
                THEN 'Spices & Blends'
        END AS category

    FROM product_category_tree AS pct

    WHERE pct.category_id IN (
        'pcat_01KRBDPYHB2C5CPV03929QQQFK',
        'pcat_01KRCEBEX5NSMTW7FJZP9QN961',
        'pcat_01KRCCWXM5B9H4N1SGPMKAWPGV',
        'pcat_01KRCE52VH4P3831FKKE023DQX',
        'pcat_01KRCDZ157N2T2F2AZR5ZBJT4K',
        'pcat_01KRCEDKVETGQF0405BQXB53X7',
        'pcat_01KRCE2TNZWTD5G04VTD5AA749'
    )

    ORDER BY
        pct.product_id,

        CASE pct.category_id
            WHEN 'pcat_01KRBDPYHB2C5CPV03929QQQFK' THEN 1
            WHEN 'pcat_01KRCEBEX5NSMTW7FJZP9QN961' THEN 2
            WHEN 'pcat_01KRCCWXM5B9H4N1SGPMKAWPGV' THEN 3
            WHEN 'pcat_01KRCE52VH4P3831FKKE023DQX' THEN 4
            WHEN 'pcat_01KRCDZ157N2T2F2AZR5ZBJT4K' THEN 5
            WHEN 'pcat_01KRCEDKVETGQF0405BQXB53X7' THEN 6
            WHEN 'pcat_01KRCE2TNZWTD5G04VTD5AA749' THEN 7
            ELSE 99
        END
),

partial_cancelled AS (
    SELECT
        line_item_id,
        SUM(cancelled_quantity)::bigint
            AS partial_cancelled_qty
    FROM partial_order_cancellation_item
    GROUP BY
        line_item_id
),

order_category_incidence AS (
    /*
     * DISTINCT is important.
     *
     * If one customer's second order contains three Atta products,
     * that customer still counts once for Atta at purchase #2.
     */
    SELECT DISTINCT
        so.customer_id,
        so.purchase_number,

        COALESCE(
            cpc.category,
            'Unmapped'
        ) AS category

    FROM sequenced_orders AS so

    JOIN line_item AS li
        ON li.order_id = so.order_id

    LEFT JOIN partial_cancelled AS pcq
        ON pcq.line_item_id = li.id

    LEFT JOIN product_variant AS pv
        ON pv.id = li.variant_id

    LEFT JOIN canonical_product_category AS cpc
        ON cpc.product_id = pv.product_id

    WHERE
        so.purchase_number <= ($4::bigint)
        AND li.variant_id IS NOT NULL
        AND li.is_return = false
        AND GREATEST(
            li.quantity::bigint
                - COALESCE(
                    pcq.partial_cancelled_qty,
                    0
                ),
            0
        ) > 0
),

ordinal_population AS (
    /*
     * Denominator:
     *
     * all selected customers who reached purchase N,
     * regardless of what category their surviving merchandise maps to.
     */
    SELECT
        purchase_number,
        COUNT(DISTINCT customer_id)::bigint
            AS customers_reaching_purchase
    FROM sequenced_orders
    WHERE
        purchase_number <= ($4::bigint)
    GROUP BY
        purchase_number
),

category_counts AS (
    SELECT
        purchase_number,
        category,
        COUNT(DISTINCT customer_id)::bigint
            AS customers_buying_category
    FROM order_category_incidence
    GROUP BY
        purchase_number,
        category
)

SELECT
    cc.purchase_number,
    cc.category,
    op.customers_reaching_purchase,
    cc.customers_buying_category,

    ROUND(
        (
            100.0
            * cc.customers_buying_category::numeric
            / NULLIF(
                op.customers_reaching_purchase::numeric,
                0
            )
        ),
        2
    ) AS category_penetration_pct

FROM category_counts AS cc

JOIN ordinal_population AS op
    ON op.purchase_number = cc.purchase_number

ORDER BY
    cc.purchase_number ASC,
    cc.customers_buying_category DESC,
    cc.category ASC
"#;

fn summarize_customer_analysis(plan: &CustomerAnalysisPlan, result: &QueryResult) -> String {
    match &plan.analysis {
        CustomerAnalysis::PurchaseProgression {
            subject: CustomerProgressionSubject::Category,
            max_purchase_number,
        } => summarize_category_progression(result, *max_purchase_number),

        _ => {
            format!("Customer analysis returned {} rows.", result.row_count)
        }
    }
}

fn summarize_category_progression(result: &QueryResult, max_purchase_number: u32) -> String {
    if result.rows.is_empty() {
        return "No qualifying loyal-customer category progression was found for this period."
            .to_owned();
    }

    let cohort_size = result.rows.iter().find_map(|row| {
        let purchase_number = row.get("purchase_number")?.as_u64()?;

        (purchase_number == 1)
            .then(|| row.get("customers_reaching_purchase")?.as_u64())
            .flatten()
    });

    let mut answer = match cohort_size {
        Some(customer_count) => format!(
            "Loyal-customer category progression across the first {} lifetime qualifying purchases for {} customers.",
            max_purchase_number,
            customer_count,
        ),

        None => format!(
            "Loyal-customer category progression across the first {} lifetime qualifying purchases.",
            max_purchase_number,
        ),
    };

    let mut top_category_lines = Vec::new();

    for purchase_number in 1..=max_purchase_number {
        let top_row = result
            .rows
            .iter()
            .filter(|row| {
                row.get("purchase_number").and_then(Value::as_u64)
                    == Some(u64::from(purchase_number))
            })
            .max_by(|left, right| {
                let left_value = left
                    .get("category_penetration_pct")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);

                let right_value = right
                    .get("category_penetration_pct")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);

                left_value
                    .partial_cmp(&right_value)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

        let Some(top_row) = top_row else {
            continue;
        };

        let category = top_row
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("Unknown");

        let penetration = top_row
            .get("category_penetration_pct")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);

        top_category_lines.push(format!(
            "{} purchase: {} ({:.2}%)",
            ordinal_label(purchase_number),
            category,
            penetration,
        ));
    }

    if !top_category_lines.is_empty() {
        answer.push_str("\n\nTop category at each purchase:");

        for line in top_category_lines {
            answer.push_str("\n");
            answer.push_str(&line);
        }
    }

    answer.push_str(
        "\n\nCategory percentages are incidence within each purchase number, so they can total more than 100% when one order contains multiple categories.",
    );

    answer
}

fn ordinal_label(value: u32) -> String {
    let suffix = if (11..=13).contains(&(value % 100)) {
        "th"
    } else {
        match value % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };

    format!("{value}{suffix}")
}

fn safe_parameters_for_log(parameters: &[QueryParameter]) -> String {
    let values = parameters
        .iter()
        .map(|parameter| match parameter {
            QueryParameter::String(value) => Value::String(value.clone()),

            QueryParameter::Integer(value) => Value::from(*value),

            QueryParameter::Float(value) => Value::from(*value),

            QueryParameter::Boolean(value) => Value::from(*value),

            QueryParameter::Null => Value::Null,
        })
        .collect::<Vec<_>>();

    serde_json::to_string(&values).unwrap_or_else(|_| "[\"<unavailable>\"]".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{CustomerProgressionSubject, CustomerSegment, TimePeriod};
    use chrono::NaiveDate;
    use serde_json::{Map, Value};

    fn loyal_category_plan() -> CustomerAnalysisPlan {
        CustomerAnalysisPlan {
            period: TimePeriod {
                label: "Lifetime through July 2026".to_owned(),

                start: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),

                end_exclusive: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            },

            segment: CustomerSegment::Loyal,

            analysis: CustomerAnalysis::PurchaseProgression {
                subject: CustomerProgressionSubject::Category,

                max_purchase_number: 3,
            },
        }
    }

    #[test]
    fn loyal_progression_uses_trailing_four_month_eligibility_window() {
        let compiled = compile_customer_analysis(&loyal_category_plan())
            .expect("loyal category progression should compile");

        assert_eq!(
            compiled.eligibility_start,
            NaiveDate::from_ymd_opt(2026, 4, 1,).unwrap(),
        );

        assert_eq!(compiled.parameters.len(), 4);

        assert!(
            matches!(
                &compiled.parameters[0],
                QueryParameter::String(value)
                    if value == "2026-08-01"
            ),
            "expected $1 to be the historical exclusive boundary",
        );

        assert!(
            matches!(
                &compiled.parameters[1],
                QueryParameter::String(value)
                    if value == "2026-04-01"
            ),
            "expected $2 to be the four-month loyalty eligibility start",
        );

        assert!(
            matches!(
                &compiled.parameters[2],
                QueryParameter::Integer(value)
                    if *value == 3
            ),
            "expected $3 to require at least three qualifying orders",
        );

        assert!(
            matches!(
                &compiled.parameters[3],
                QueryParameter::Integer(value)
                    if *value == 3
            ),
            "expected $4 to sequence through the third purchase",
        );
    }

    #[test]
    fn category_progression_sql_sequences_lifetime_orders_deterministically() {
        let compiled = compile_customer_analysis(&loyal_category_plan())
            .expect("loyal category progression should compile");

        let normalized = compiled.sql.to_ascii_lowercase();

        assert!(normalized.contains("row_number() over"));

        assert!(normalized.contains("partition by qh.customer_id"));

        assert!(normalized.contains("order by qh.created_at, qh.order_id"));

        /*
         * Lifetime sequence intentionally has only an upper
         * historical boundary. It must not reuse the loyalty
         * eligibility lower bound.
         */
        assert!(normalized.contains("from \"order\" as o"));

        assert!(normalized.contains("join eligible_customers as ec"));
    }

    #[test]
    fn category_progression_sql_uses_canonical_category_mapping_and_unmapped_fallback() {
        let compiled = compile_customer_analysis(&loyal_category_plan())
            .expect("loyal category progression should compile");

        let normalized = compiled.sql.to_ascii_lowercase();

        assert!(normalized.contains("product_category_product"));

        assert!(normalized.contains("product_category_tree"));

        assert!(normalized.contains("atta & daliya"));

        assert!(normalized.contains("rice & pulses"));

        assert!(normalized.contains("'unmapped'"));
    }

    #[test]
    fn category_progression_sql_does_not_project_customer_pii() {
        let compiled = compile_customer_analysis(&loyal_category_plan())
            .expect("loyal category progression should compile");

        let normalized = compiled.sql.to_ascii_lowercase();

        for prohibited in [
            "first_name",
            "last_name",
            "email",
            "phone",
            "address",
            "date_of_birth",
        ] {
            assert!(
                !normalized.contains(prohibited),
                "customer analysis SQL must not contain {prohibited}",
            );
        }
    }

    #[test]
    fn category_progression_sql_passes_read_only_ast_validation() {
        let compiled = compile_customer_analysis(&loyal_category_plan())
            .expect("loyal category progression should compile");

        let validated = validate_sql(&compiled.sql)
            .expect("deterministic customer SQL should pass AST validation");

        assert_eq!(validated.parameter_count(), compiled.parameters.len(),);
    }

    #[test]
    fn category_progression_summary_reports_top_category_per_purchase() {
        let mut first_atta = Map::new();

        first_atta.insert("purchase_number".to_owned(), Value::from(1));

        first_atta.insert("category".to_owned(), Value::from("Atta & Daliya"));

        first_atta.insert("customers_reaching_purchase".to_owned(), Value::from(100));

        first_atta.insert("customers_buying_category".to_owned(), Value::from(72));

        first_atta.insert("category_penetration_pct".to_owned(), Value::from(72.0));

        let mut second_rice = Map::new();

        second_rice.insert("purchase_number".to_owned(), Value::from(2));

        second_rice.insert("category".to_owned(), Value::from("Rice & Pulses"));

        second_rice.insert("customers_reaching_purchase".to_owned(), Value::from(100));

        second_rice.insert("customers_buying_category".to_owned(), Value::from(44));

        second_rice.insert("category_penetration_pct".to_owned(), Value::from(44.0));

        let mut third_bakery = Map::new();

        third_bakery.insert("purchase_number".to_owned(), Value::from(3));

        third_bakery.insert("category".to_owned(), Value::from("Bakery"));

        third_bakery.insert("customers_reaching_purchase".to_owned(), Value::from(100));

        third_bakery.insert("customers_buying_category".to_owned(), Value::from(31));

        third_bakery.insert("category_penetration_pct".to_owned(), Value::from(31.0));

        let result = QueryResult {
            columns: vec![
                "purchase_number".to_owned(),
                "category".to_owned(),
                "customers_reaching_purchase".to_owned(),
                "customers_buying_category".to_owned(),
                "category_penetration_pct".to_owned(),
            ],

            rows: vec![first_atta, second_rice, third_bakery],

            row_count: 3,
            truncated: false,
            duration_ms: 1,
            data_as_of: None,
        };

        let summary = summarize_category_progression(&result, 3);

        assert!(summary.contains("100 customers"));

        assert!(summary.contains("1st purchase: Atta & Daliya (72.00%)"));

        assert!(summary.contains("2nd purchase: Rice & Pulses (44.00%)"));

        assert!(summary.contains("3rd purchase: Bakery (31.00%)"));

        assert!(summary.contains("more than 100%"));
    }
}
