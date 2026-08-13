# Money domain

Use this domain for order value, qualifying order value, captured value, refunds, net-collected operational value, AOV, and monetary comparisons.

All commerce money values are stored in integer minor units unless a verified field explicitly states otherwise.

For INR:

```text
100 minor units = ₹1
````

Keep money in minor units through SQL and deterministic Rust calculations.

Convert to INR exactly once at presentation.

## MONEY PRINCIPLES

Do not use one generic concept called `revenue`.

Different questions require different monetary concepts.

Canonical concepts include:

```text
GROSS_ITEM_VALUE
COMPUTED_PLACED_VALUE
PLACED_ORDER_VALUE
QUALIFYING_ORDER_VALUE
CAPTURED_VALUE
PROCESSED_REFUND
NET_COLLECTED_VALUE
PLACED_AOV
QUALIFYING_AOV
REALIZED_AOV
```

Use the concept matching the user's wording.

Do not silently substitute one for another.

## ORDER GRAIN

Monetary order analytics use:

```text
order.id
```

as canonical order grain.

Before aggregating money across orders:

1. select or calculate one applicable monetary value per order;
2. reduce child tables such as payments, refunds, adjustments, and partial cancellations to order grain;
3. only then aggregate across orders.

Never sum payment amounts after joining raw payment rows to line items or other one-to-many children.

## TABLE: line_item

Relevant monetary columns:

### li.id

Meaning:

canonical line-item identity

### li.order_id

Meaning:

relationship to order

```text
line_item.order_id
many → one
order.id
```

### li.unit_price

Meaning:

historical transaction line unit price

Use for historical line value.

Do not replace historical charged price with current product/variant pricing.

### li.quantity

Meaning:

original ordered quantity

This value is not automatically reduced by partial cancellation.

Use the cancellation-adjusted quantity/value concepts where required.

## TABLE: line_item_adjustment

Use the verified adjustment relationship supplied by the active schema.

Adjustments are line-level value changes.

When computing adjusted historical line value:

```text
gross line value
-
absolute line adjustments
```

Preaggregate adjustments to line-item grain before combining with other one-to-many data.

Do not let multiple adjustment rows multiply line quantity.

## TABLE: shipping_method

Use the verified order-to-shipping relationship supplied by the schema/context.

Shipping amount may contribute to computed placed order value where that fallback definition is required.

Preaggregate/resolve shipping at order grain.

## TABLE: payment

Payment is a one-to-many child of order.

Relationship:

```text
payment.order_id
many → one
order.id
```

An order may contain multiple payment rows.

Therefore:

```text
SUM(payment.amount)
```

across a raw order/payment join is NOT automatically the correct historical order value.

A single applicable payment must be selected at order grain first.

## PAYMENT SELECTION

Current pragmatic historical payment selection:

select the latest applicable active/non-cancelled payment for each order according to the verified payment-state fields in the supplied schema.

This is an operational historical rule, not accounting truth.

Use an order-grain CTE/window function before joining payments to other child tables.

Conceptual pattern:

```sql
WITH ranked_payments AS (
  SELECT
    p.order_id,
    p.amount,
    ROW_NUMBER() OVER (
      PARTITION BY p.order_id
      ORDER BY p.created_at DESC, p.id DESC
    ) AS rn
  FROM payment AS p
  WHERE <APPLICABLE_PAYMENT_FILTER>
),
selected_payment AS (
  SELECT
    order_id,
    amount
  FROM ranked_payments
  WHERE rn = 1
)
```

Use the exact verified active/cancelled payment columns and values supplied by the schema.

Do not invent payment-state fields.

## CONCEPT: GROSS_ITEM_VALUE

Definition:

historical line-item value before adjustments/cancellations/shipping.

Canonical calculation:

```sql
SUM(
  COALESCE(li.unit_price, 0)::bigint
  * COALESCE(li.quantity, 0)::bigint
)
```

Use only when the user explicitly asks for concepts such as:

* gross item value
* gross merchandise line value
* pre-adjustment item value

Do not call this:

* qualifying value
* captured value
* net collected value
* accounting revenue

## CONCEPT: COMPUTED_PLACED_VALUE

Operational fallback/reconciliation metric.

Conceptually:

```text
gross item value
- absolute line adjustments
+ shipping
```

Use when:

* no better verified order-level payment value is available;
* the request explicitly asks for computed/cart/order value;
* reconciliation requires comparison against payment value.

This is medium-confidence operational logic.

Do not automatically use it when a selected payment amount is available for the requested concept.

## CONCEPT: PLACED_ORDER_VALUE

Definition:

selected historical order-level payment amount for a placed order.

Preferred source:

```text
selected applicable payment amount
```

at order grain.

Population:

`PLACED_ORDER`

A later-cancelled order remains part of a placed-order metric if the user asks for placed order value.

Do not automatically apply qualifying cancellation semantics.

If selected payment truth is unavailable for a historical order, the documented computed placed value may be used as a fallback and should remain identifiable as a fallback.

## CONCEPT: QUALIFYING_ORDER_VALUE

Definition:

placed order value for a qualifying/non-fully-cancelled order, reduced by applicable partial-cancellation value.

Conceptually:

```text
selected placed order value
-
partial cancellation value
```

Population:

`QUALIFYING_ORDER`

Fully cancelled orders contribute zero because they are excluded from the qualifying population.

Partial cancellations do not fully cancel the order.

Do not use raw payment amount alone when partial cancellation value must be removed.

## PARTIAL CANCELLATION VALUE

Use the verified partial-cancellation workflow supplied by the active schema/context.

Preaggregate cancellation amount to:

```text
order_id
```

before joining to selected payment.

Do not join raw partial-cancellation rows directly to:

* payment
* line_item
* refund

and then sum money.

Conceptual structure:

```sql
partial_cancel_by_order AS (
  SELECT
    <order_id>,
    SUM(<verified_cancelled_value>) AS partial_cancel_value
  FROM <verified_partial_cancellation_source>
  GROUP BY <order_id>
)
```

Use only verified tables/columns.

Do not infer cancellation value from current price-list state.

## CONCEPT: CAPTURED_VALUE

Definition:

selected order-level payment amount for orders whose canonical aggregate payment state is captured.

Canonical state source:

```text
order.payment_status
```

Canonical population concept:

```text
order.payment_status = captured
```

using the verified enum/text representation supplied by the schema.

Do not use:

```text
payment.captured_at
```

as canonical captured-value truth.

Do not sum all payment rows for a captured order.

Select the applicable order payment first.

## TABLE: refund_request

Use this as the verified operational refund workflow.

Relevant concepts:

```text
order relationship
refund amount
refund status
```

Use exact supplied physical column names.

## CONCEPT: PROCESSED_REFUND

Definition:

refund amount from the verified refund-request workflow where refund status is processed.

Canonical source:

```text
refund_request.refund_amount
```

with:

```text
refund_request.status = processed
```

using the verified exact representation.

Preaggregate processed refunds to order grain when combining with payment/order values.

Do not use:

* `payment.amount_refunded`
* native refund rows

as canonical current processed-refund truth.

## CREDIT NOTES

Credit notes are documentary/accounting-support records.

They must not automatically be added as a second monetary deduction when they represent the same underlying refund/cancellation event.

Do not calculate:

```text
captured
- processed refund
- credit note
```

unless the business definition explicitly proves that the credit note is a separate financial event.

Avoid double subtraction.

## CONCEPT: NET_COLLECTED_VALUE

Operational approximation:

```text
captured value
-
processed refunds
```

at order grain.

This may also be described as:

```text
net realized operational value
```

when the user asks for a collected/realized operational measure.

This is NOT accounting-recognized revenue.

Do not label it:

* accounting revenue
* recognized revenue
* GST-exclusive revenue

## AOV

AOV must state which value/population is used.

There is no single universal AOV.

## CONCEPT: PLACED_AOV

Definition:

```text
SUM(PLACED_ORDER_VALUE)
/
COUNT(PLACED_ORDER)
```

Use when the user explicitly asks for placed-order AOV.

Placed population includes later-cancelled orders unless the request says otherwise.

## CONCEPT: QUALIFYING_AOV

Default operational AOV when the user asks for qualifying/non-cancelled/sales AOV.

Definition:

```text
SUM(QUALIFYING_ORDER_VALUE)
/
COUNT(QUALIFYING_ORDER)
```

Both numerator and denominator must use the same qualifying order population.

Do not calculate qualifying AOV as:

```text
gross line value / order count
```

Do not use:

```text
AVG(payment.amount)
```

over raw payment rows.

## RECIPE: QUALIFYING AOV

Use explicit order-grain stages.

Conceptual structure:

```sql
WITH selected_payment AS (
  SELECT
    ...
  FROM ...
),
partial_cancel_by_order AS (
  SELECT
    ...
  FROM ...
),
qualifying_order_values AS (
  SELECT
    o.id AS order_id,
    (
      COALESCE(sp.amount, <OPTIONAL_VERIFIED_FALLBACK>)
      - COALESCE(pc.partial_cancel_value, 0)
    ) AS qualifying_order_value
  FROM "order" AS o
  LEFT JOIN selected_payment AS sp
    ON sp.order_id = o.id
  LEFT JOIN partial_cancel_by_order AS pc
    ON pc.order_id = o.id
  WHERE <QUALIFYING_ORDER>
    AND <TIME_RANGE>
)
SELECT
  SUM(qualifying_order_value) AS qualifying_value,
  COUNT(*) AS qualifying_order_count,
  SUM(qualifying_order_value)::numeric
    / NULLIF(COUNT(*), 0) AS qualifying_aov
FROM qualifying_order_values
```

Use the actual verified payment/cancellation fields from the active schema.

Do not invent fallback expressions.

## CONCEPT: REALIZED_AOV

Operational realized AOV:

```text
SUM(NET_COLLECTED_VALUE)
/
COUNT(relevant realized/captured orders)
```

Use only when the user explicitly asks for:

* realized AOV
* collected AOV
* captured-minus-refunds AOV

State the operational definition.

Do not silently use it for generic AOV.

## MONTHLY AOV

For:

`qualifying AOV by month`

first calculate one qualifying order value per order.

Then group those order-grain rows by the order's IST business month.

Conceptual structure:

```sql
WITH qualifying_order_values AS (
  SELECT
    o.id AS order_id,
    date_trunc(
      'month',
      timezone('Asia/Kolkata', o.created_at)
    )::date AS month,
    <QUALIFYING_ORDER_VALUE> AS order_value
  FROM "order" AS o
  ...
  WHERE <QUALIFYING_ORDER>
    AND <TIME_RANGE>
)
SELECT
  month,
  COUNT(*) AS qualifying_order_count,
  SUM(order_value) AS qualifying_order_value,
  SUM(order_value)::numeric
    / NULLIF(COUNT(*), 0) AS qualifying_aov
FROM qualifying_order_values
GROUP BY month
ORDER BY month
```

Do not group raw payments by month after joining child tables.

## COMPARISONS

Examples:

`Compare qualifying AOV in June vs July`

→ preserve both periods separately.

`Qualifying AOV month by month January through July`

→ monthly time series.

`Captured value by payment status`

→ grouped result, if the metric/request is semantically valid.

Words such as:

* how much
* what was
* total

do not override explicit comparison/grouping intent.

## MONEY + PRODUCT ANALYSIS

When combining money and product/line-item data, protect order/payment grain.

Unsafe:

```text
order
JOIN payment
JOIN line_item
```

then:

```text
SUM(payment.amount)
```

because payment amount repeats per line item.

If allocating order value to products is requested, use an explicitly defined allocation rule.

Do not invent proportional attribution.

## MONEY + REFUND ANALYSIS

Preaggregate:

```text
selected payment → one row/order
processed refund → one row/order
partial cancellation → one row/order
```

then join.

Do not combine raw many-side rows.

## MONEY + FULFILLMENT ANALYSIS

If monetary metrics are grouped by fulfillment/delivery state:

1. reduce fulfillment to the required order-level concept;
2. select one monetary value per order;
3. join at order grain.

Do not raw-join payment and fulfillment rows.

## MINOR UNITS

SQL and Rust should retain integer minor units where possible.

Example:

```text
12345
```

means:

```text
₹123.45
```

for INR presentation.

Do not divide by 100 in SQL and divide again in UI.

Do not mix values that have already been converted with raw minor-unit values.

## B2B MONEY

Explicit B2B orders may be identified using the verified B2B order semantics from `orders.md`.

Historical B2B monetary truth remains caveated where negotiated/current price-list state cannot prove the historical charged amount.

Prefer historical transaction/payment values.

Do not reconstruct historical B2B value from current price lists.

## UNSUPPORTED ACCOUNTING METRICS

The current analytics layer does not establish authoritative accounting truth for:

* recognized revenue
* accounting revenue
* GST-exclusive revenue
* tax-exclusive revenue
* authoritative GST reporting
* profit
* contribution margin

unless a future verified domain explicitly defines them.

If asked, state the limitation or use a clearly named supported operational metric.

Do not silently rename:

```text
captured value
```

or:

```text
net collected value
```

as accounting revenue.

## RESULT SHAPE

Examples:

`What was qualifying AOV yesterday?`

→ scalar

`Qualifying AOV by month`

→ time series

`Compare June and July qualifying AOV`

→ comparison/time series

`Net collected value by store`

→ grouped result

`Top stores by captured value`

→ ranking

Preserve requested shape.

## DO NOT USE

Do NOT:

* sum raw payment rows as order value
* use `payment.captured_at` as canonical capture truth
* use `payment.amount_refunded` as canonical processed-refund truth
* use native refund rows as current canonical refund workflow
* subtract a credit note and refund twice for the same event
* reconstruct historical price from current catalog/price-list state
* divide by 100 more than once
* call gross line value qualifying AOV
* call operational collected value accounting revenue
* join payment + line_item + refund/fulfillment raw rows before aggregation
* use current B2B pricing as proven historical charged value

## QUERY CONSTRUCTION CHECKLIST

For a money query determine:

```text
1. which money concept?
2. placed or qualifying population?
3. one applicable payment selected per order?
4. partial cancellation adjustment required?
5. processed refund adjustment required?
6. order grain protected?
7. comparison/grouping requested?
8. minor units preserved?
9. accounting metric unsupported?
10. cross-domain child joins preaggregated?
```

Then compose SQL from the canonical concepts and recipes above.

If the requested monetary truth is not defined here or in the supplied schema/context, fail planning rather than invent it.