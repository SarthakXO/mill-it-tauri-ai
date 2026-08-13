# Forecasting domain

Use this domain only for predictive requests or for historical observations required by deterministic Rust forecasting/reorder operations.

Qwen interprets forecasting intent.

Rust is prediction authority.

Qwen must NOT generate forecast or reorder numbers itself.

## ROLE

This domain defines:

- forecast intent
- training-window semantics
- forecast-horizon semantics
- historical observation requirements
- product/variant demand inputs
- reorder-history inputs
- deterministic routing rules

Use SQL only to retrieve historical observations needed by the deterministic Rust prediction path.

Do not use generated SQL itself as a forecasting model.

## PREDICTIVE ROUTING

Requests containing concepts such as:

- forecast
- predict
- expected future orders
- expected demand
- next N days
- next month
- likely to reorder
- due to reorder
- expected reorder date
- reorder demand
- overdue reorder

must route to the existing deterministic predictive operation when available.

Examples:

```text
Forecast orders for the next 14 days
→ order forecasting
````

```text
Forecast variant demand for next month
→ product/variant forecasting
```

```text
Which customers are likely due to reorder?
→ reorder_demand
```

Do not generate future numbers directly from Qwen.

## FORECAST PLAN

Qwen should return semantic forecast instructions.

Example:

```json
{
  "metric": "order_count",
  "population": "placed",
  "training_window": {
    "kind": "complete_calendar_months",
    "count": 6
  },
  "forecast_horizon": {
    "unit": "days",
    "count": 14
  },
  "grain": "day"
}
```

Qwen determines:

* requested metric
* requested population
* semantic training-window type
* training-window count
* forecast-horizon type
* forecast-horizon count
* requested grain

Rust resolves concrete dates.

## TRAINING WINDOW TYPES

Supported concepts include:

```text
complete_calendar_months
complete_calendar_weeks
complete_days
```

### complete_calendar_months

Example:

`last 6 complete calendar months`

means:

six fully completed calendar months before the current incomplete month.

Rust resolves the exact start and exclusive end dates.

Qwen must not invent the concrete dates.

### complete_calendar_weeks

Example:

`last 8 complete weeks`

means:

eight fully completed Monday-start calendar weeks.

Rust resolves the boundaries.

### complete_days

Example:

`last 90 complete days`

means:

90 fully completed IST calendar days.

The current IST day is not a complete historical day.

## FORECAST HORIZON

Supported horizon concepts include:

```text
days
calendar_months
```

Example:

```json
{
  "unit": "days",
  "count": 14
}
```

must produce exactly 14 forecast observations at daily grain.

Do not silently replace the requested horizon with:

* 30 days
* one month
* a default horizon
* another model-selected duration

## NEXT-N-DAYS POLICY

Use one deterministic application policy.

Recommended policy:

```text
next N days
→ next N complete future IST calendar days
→ begin tomorrow
```

Example:

if today is August 13:

```text
next 14 days
→ August 14 through August 27
→ exactly 14 rows
```

If the user explicitly says:

* including today
* today plus the next 13 days

use the explicit application-supported include-today/nowcast path.

Qwen must not decide ad hoc whether today is included.

## HISTORICAL COMPLETENESS

The current IST day is incomplete.

Do not use it as a complete historical training observation.

The latest observed database timestamp is freshness evidence only.

It is NOT an authoritative completeness watermark.

There is currently no verified ingestion-completeness watermark.

Rust must use the configured stable historical cutoff.

## ORDER FORECAST POPULATIONS

Forecast population must match the user's wording.

### PLACED_ORDER_FORECAST

Examples:

* forecast orders
* expected orders
* order volume forecast
* predict number of orders

Population:

`PLACED_ORDER`

Use all placed orders in the historical interval.

Do not automatically remove later-cancelled orders.

### QUALIFYING_ORDER_FORECAST

Examples:

* forecast qualifying orders
* forecast non-cancelled sales
* expected qualifying order volume

Population:

`QUALIFYING_ORDER`

Use the canonical qualifying-order predicate.

Do not silently convert:

```text
placed → qualifying
```

or:

```text
qualifying → placed
```

## HISTORICAL ORDER OBSERVATIONS

For daily order forecasting, SQL should return one historical aggregate observation per complete IST calendar day.

Typical structure:

```text
date
order_count
```

Historical SQL should:

1. use the requested population;
2. use the resolved training boundaries supplied by Rust;
3. group by IST business date;
4. remain read only.

Example conceptual query:

```sql
SELECT
  timezone('Asia/Kolkata', o.created_at)::date AS business_date,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE <REQUESTED_POPULATION>
  AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
GROUP BY business_date
ORDER BY business_date
```

For placed-order forecast, `<REQUESTED_POPULATION>` has no cancellation filter.

For qualifying-order forecast, use `QUALIFYING_ORDER`.

Rust owns zero-date filling.

Do not invent missing zero-demand days in SQL unless the existing deterministic operation explicitly expects SQL-generated date series.

## ZERO-DEMAND DAYS

Historical time series must represent dates with zero observations.

If SQL returns event-only dates, Rust must add missing complete dates with value zero before model fitting.

Do not let missing dates disappear from the forecasting series.

## PRODUCT AND VARIANT FORECASTING

Product and variant demand use different analytical grains.

### VARIANT_DEMAND_FORECAST

Identity:

```text
variant_id
```

Use:

`line_item.variant_id`

Do not use literal:

`product_variant.sku`

as the forecasting key.

### PRODUCT_DEMAND_FORECAST

Identity:

```text
product_id
```

Roll all applicable variants into product grain before producing the historical product-demand series.

Use the relationships supplied by `products.md`.

Do not answer product demand at variant grain.

## NET QUANTITY INPUT

For qualifying product/variant demand forecasting, use `NET_LINE_QUANTITY` from `products.md`.

Conceptually:

```text
original line quantity
- partial-cancelled quantity
```

Fully cancelled orders contribute no qualifying demand.

Do not use raw:

```text
SUM(line_item.quantity)
```

when cancellation-adjusted demand is requested.

Returns remain a separate caveat unless a verified return semantic is supplied.

## HISTORICAL PRODUCT LABELS

Forecast identity comes from IDs.

Use:

```text
variant_id
product_id
```

Do not use titles as forecast identity.

`line_item.title`

may be used only as historical display metadata.

Do not:

* split one forecast series because a title changed;
* merge different variants because titles look similar;
* remove historical transactions because current catalog enrichment is missing.

## B2B

Canonical explicit B2B marker:

```sql
o.is_b2b IS TRUE
```

B2B may have different demand behavior.

If the user requests:

* B2B forecast
* B2C forecast
* segmented B2B/B2C forecast

preserve that population.

If B2B materially affects a generic forecast and the deterministic operation supports segmentation, Rust may segment it.

Do not derive historical B2B identity from current customer/client mappings.

## RUST FORECAST RESPONSIBILITIES

Rust owns:

* date resolution
* complete-period boundaries
* stable historical cutoff
* missing-date generation
* candidate forecast models
* moving averages
* weekday baselines
* trend calculations
* rolling backtests
* MAE
* WAPE
* candidate selection
* forecast values
* forecast ranges
* forecast totals
* busiest-day ranking
* comparison against prior actual periods
* projection persistence/evaluation

Qwen must not calculate or override these.

## FORECAST MODEL SELECTION

When multiple candidate models exist:

```text
historical observations
→ Rust candidate models
→ Rust backtest
→ Rust scoring
→ Rust selected model
```

Qwen may explain the selected model only after Rust provides the result.

Qwen does not choose the winning model from intuition.

## PREVIOUS-PERIOD COMPARISON

Example request:

`Forecast the next 14 days and compare with the previous 14 complete days`

Rust resolves:

```text
forecast horizon
= exactly 14 future forecast observations
```

and:

```text
comparison period
= immediately preceding 14 complete historical days
```

Rust calculates:

* forecast total
* previous actual total
* absolute change
* percentage change

Qwen may explain only the approved results.

Do not compare unequal-length periods unless the user explicitly requests it.

## BUSIEST EXPECTED DAYS

Example:

`Show the 3 busiest expected days`

This is an output transformation of the full forecast.

Rust should:

1. produce the complete requested forecast horizon;
2. rank those forecast observations;
3. return the top 3.

Do not replace the full forecast with a separate busy-day-only prediction.

## REORDER ROUTING

Explicit reorder requests route to deterministic reorder functionality.

Typical trigger concepts:

```text
likely to reorder
due to reorder
expected reorder
expected reorder date
reorder interval
overdue reorder
reorder demand
likely to buy again
```

Preferred specialized operation:

```text
reorder_demand
```

Qwen may interpret:

* requested horizon
* requested filters
* requested output fields

Rust owns prediction.

## REORDER HISTORICAL INPUT

Canonical reorder grain:

```text
customer_id
+
variant_id
```

Historical observations should contain:

```text
customer_id
variant_id
qualifying purchase date
net quantity
```

Use only qualifying purchases.

Fully cancelled orders must NOT:

* create a purchase event
* reset last purchase
* contribute an interval
* contribute quantity

Use net quantity when partial cancellation applies.

## REORDER STATISTICS

Rust owns calculation of:

* number of historical purchases
* reorder interval sequence
* average interval
* median interval
* interval variability
* last purchase date
* days since last purchase
* expected reorder date
* overdue days
* average historical quantity
* median historical quantity
* confidence score
* confidence label
* expected reorder quantity
* product/variant reorder-demand aggregation

Qwen must not invent these values.

If the user asks specifically for:

```text
average quantity
```

return average quantity.

Do not silently label median as average.

## REORDER CONFIDENCE

Confidence must be deterministic.

Possible inputs include:

* number of qualifying purchases
* number of observed reorder intervals
* interval variability
* recency
* quantity consistency

Rust may return:

```json
{
  "confidence_score": 0.82,
  "confidence": "high",
  "confidence_reasons": [
    "5 qualifying purchases",
    "4 observed reorder intervals",
    "low interval variability"
  ]
}
```

Qwen may explain these supplied reasons.

Qwen must not generate an independent confidence score.

## FORECAST/REORDER ANALYSIS

Qwen may analyze prediction output only after Rust has produced deterministic facts.

Allowed analyst inputs:

* approved facts
* aggregate historical statistics
* forecast rows
* forecast comparison facts
* aggregate reorder-demand summaries

Do NOT send:

* customer names
* phone
* email
* other customer PII

to the analyst.

## ANALYSIS RULES

Qwen may:

* summarize trends
* describe historical patterns
* point out high/low periods
* suggest investigations
* explain deterministic model outputs

Qwen may NOT:

* invent forecast numbers
* recalculate forecast totals
* invent percentages
* change provided values
* invent causal explanations
* invent reorder probabilities
* invent confidence

Distinguish:

```text
FACT
OBSERVATION
INVESTIGATION
```

Do not present an observed association as a proven cause.

## SIMPLE OUTPUTS

If the predictive operation already returns a clear table and deterministic summary, Insight Analyst is optional.

Do not invoke a second model merely to repeat:

* forecast values
* top forecast days
* reorder rows
* simple deterministic totals

Use the analyst only when interpretation adds value.

## DO NOT USE

Do NOT:

* have Qwen generate future values
* have Qwen calculate forecast dates
* let Qwen invent training boundaries
* use the current partial IST day as complete history
* use literal SKU as normal forecast identity
* group forecast identity by title
* include fully cancelled orders in qualifying demand/reorder history
* silently substitute placed and qualifying populations
* silently change requested horizon length
* pass customer PII to the analyst
* use SQL output itself as the prediction model

## QUERY CONSTRUCTION CHECKLIST

For forecasting/reorder requests determine:

```text
1. forecast or reorder?
2. metric?
3. placed or qualifying population?
4. product or variant grain?
5. training-window semantic type?
6. training-window count?
7. forecast horizon type/count?
8. complete-period requirement?
9. historical SQL observations needed?
10. specialized Rust operation?
```

Then use SQL only to retrieve the verified historical observations required by the deterministic Rust operation.

If required predictive semantics are not defined here or in another selected domain pack, fail planning rather than invent them.