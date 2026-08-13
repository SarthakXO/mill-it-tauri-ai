# Orders domain

Use this domain for placed-order counts, qualifying-order counts, order comparisons, operational status, native status, payment status, native fulfillment status, store/B2B segmentation, and safe order lists.

The canonical analytical order grain is:

`order.id`

## TABLE: "order"

Grain:

one row per order

Alias:

`o`

The relation name is reserved and must always be quoted:

```sql
"order"
````

## APPROVED COLUMNS

### o.id

Meaning:

canonical order identity

Use for:

* order counts
* order-level grouping
* joins from child tables

When joins may multiply rows, prefer:

```sql
COUNT(DISTINCT o.id)
```

or preaggregate children to order grain first.

### o.created_at

Meaning:

physical order creation / placed timestamp

Type:

`timestamptz`

Business timezone:

`Asia/Kolkata`

Canonical business-date expression:

```sql
timezone('Asia/Kolkata', o.created_at)
```

Use this for:

* placed-date filtering
* day/week/month grouping
* order-volume comparisons

Do not use `updated_at` as order date.

### o.updated_at

Meaning:

latest order-row update timestamp

Do NOT use as:

* placed date
* sales date
* qualifying purchase date

unless the user explicitly asks about row updates.

### o.status

Meaning:

native Medusa order lifecycle status

Use for:

* native order status questions
* canonical full-cancellation logic

Do not treat this as Mill-it operational status.

### o.zoho_order_status

Meaning:

Mill-it operational order status

Use for:

* operational status questions
* operational fulfillment/completion
* canonical full-cancellation logic

This is the default source when the user asks for Mill-it operational status without explicitly requesting native Medusa status.

### o.payment_status

Meaning:

aggregate order-level payment lifecycle

Use for:

* payment-status counts
* payment-status breakdowns
* captured-state semantics referenced by `money.md`

Do not automatically load the money domain unless a monetary value is requested.

### o.fulfillment_status

Meaning:

native Medusa aggregate fulfillment lifecycle

Use only for:

* native fulfillment status
* Medusa fulfillment-status questions

Do not treat this as actual custom delivery state.

### o.delivery_status

Do NOT use as actual delivery-state truth.

Actual delivery state is defined in `fulfillment.md` from:

`fulfillment.delivery_status`

### o.customer_id

Meaning:

canonical relationship to customer

Use for:

* unique-customer counts
* repeat behavior
* customer purchase history

Relationship:

```text
order.customer_id
many → one
customer.id
```

### o.store_id

Meaning:

stored order-level store attribution

Use for:

* store-level order comparisons
* order grouping by store
* store filtering

Use the exact schema relationship if store metadata is required.

### o.is_b2b

Meaning:

explicit B2B order marker

Canonical explicit B2B predicate:

```sql
o.is_b2b IS TRUE
```

Use for:

* B2B order count
* B2B segmentation
* B2B vs non-B2B comparisons

Do not infer historical B2B identity from current customer/client mappings when this order-level marker is available.

## CANONICAL GRAIN

Canonical order grain:

```text
o.id
```

Do not let:

* line items
* payments
* fulfillments
* refunds
* cancellations

multiply order counts.

If a child table is required, aggregate/select it to order grain first.

## CONCEPT: PLACED_ORDER

Definition:

a physical `"order"` row whose `created_at` falls inside the requested IST period.

No cancellation filter.

No operational-status filter.

No payment-status filter.

Canonical conceptual condition:

```sql
timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
```

Use resolved parameter values supplied by Rust.

Examples that normally mean `PLACED_ORDER`:

* orders
* orders placed
* total orders
* order count
* order volume
* how many orders

unless the user explicitly asks for qualifying/non-cancelled/sales behavior.

A later-cancelled order still counts as a placed order.

Do NOT silently turn placed-order queries into qualifying-order queries.

## CONCEPT: FULLY_CANCELLED_ORDER

Canonical full-cancellation predicate:

```sql
(
  lower(o.status::text) IN ('canceled', 'cancelled')
  OR upper(COALESCE(o.zoho_order_status, ''))
       IN ('CANCELED', 'CANCELLED')
)
```

Use both sources.

Do NOT use:

```text
order.canceled_at
```

as canonical cancellation truth.

## CONCEPT: QUALIFYING_ORDER

Definition:

placed order that is NOT fully cancelled.

Canonical predicate:

```sql
NOT (
  lower(o.status::text) IN ('canceled', 'cancelled')
  OR upper(COALESCE(o.zoho_order_status, ''))
       IN ('CANCELED', 'CANCELLED')
)
```

Use when the user explicitly asks for concepts such as:

* qualifying orders
* non-cancelled orders
* non-canceled orders
* sales orders
* qualifying sales
* valid purchase behavior

Do not apply this predicate to plain `orders placed` unless explicitly requested.

Partial cancellation does NOT make the whole order fully cancelled.

## CONCEPT: OPERATIONAL_STATUS

Canonical source:

```text
o.zoho_order_status
```

Use for:

* order operational status
* Mill-it order status
* Ops status
* operational lifecycle breakdown

Preserve NULL as unknown.

Do not silently map NULL to:

* pending
* processing
* fulfilled

Example grouped query concept:

```sql
SELECT
  COALESCE(o.zoho_order_status, 'UNKNOWN') AS operational_status,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE <TIME_RANGE>
GROUP BY o.zoho_order_status
ORDER BY order_count DESC
```

If presentation requires `UNKNOWN`, it may be produced as a display label while preserving underlying NULL semantics.

## CONCEPT: NATIVE_ORDER_STATUS

Canonical source:

```text
o.status
```

Use only when the user asks for:

* native order status
* Medusa status
* `order.status`

Do not substitute operational status.

## CONCEPT: OPERATIONALLY_FULFILLED

Canonical predicate:

```sql
upper(COALESCE(o.zoho_order_status, '')) = 'FULFILLED'
```

Use for phrases such as:

* fulfilled orders
* operationally fulfilled
* completed operational fulfillment

unless the user explicitly says:

* native fulfillment
* Medusa fulfillment status
* `fulfillment_status`

Example:

`total orders vs fulfilled orders on May 7`

means:

```text
total_orders
= PLACED_ORDER

fulfilled_orders
= PLACED_ORDER + OPERATIONALLY_FULFILLED
```

## RECIPE: TOTAL ORDERS VS FULFILLED

Conceptual query:

```sql
SELECT
  COUNT(*) AS total_orders,
  COUNT(*) FILTER (
    WHERE upper(COALESCE(o.zoho_order_status, '')) = 'FULFILLED'
  ) AS fulfilled_orders
FROM "order" AS o
WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
```

No cancellation filter unless the user explicitly asks for qualifying orders.

## CONCEPT: PAYMENT_STATUS

Canonical source:

```text
o.payment_status
```

Use for order counts such as:

* orders by payment status
* captured orders
* pending payment orders
* payment-status breakdown

When the user asks for monetary value associated with payment state, also use `money.md`.

Do not use child payment timestamps to replace aggregate order payment state unless `money.md` explicitly requires payment-row selection for value.

## CONCEPT: NATIVE_FULFILLMENT_STATUS

Canonical source:

```text
o.fulfillment_status
```

Use for:

* native fulfillment breakdown
* Medusa fulfillment state

Do not use this for:

* actual delivery state
* custom rider status

Those belong to `fulfillment.md`.

## CONCEPT: B2B_ORDER

Canonical predicate:

```sql
o.is_b2b IS TRUE
```

Non-B2B comparison may use:

```sql
o.is_b2b IS NOT TRUE
```

when NULL should remain outside explicit B2B.

Do not invent a customer-derived B2B classification if the request is historical order segmentation.

## TIME FILTERS

All business calendar filtering uses IST:

```sql
timezone('Asia/Kolkata', o.created_at)
```

Use half-open ranges:

```text
>= start
< end_exclusive
```

### Exact day

Example:

`May 7, 2026`

```text
start = 2026-05-07
end   = 2026-05-08
```

### Exact month

Example:

`July 2026`

```text
start = 2026-07-01
end   = 2026-08-01
```

### Yesterday

Rust should resolve:

```text
yesterday_start
today_start
```

SQL must contain both boundaries.

Do NOT generate a one-sided exact date filter.

## RECIPE: PLACED ORDER COUNT

```sql
SELECT
  COUNT(*) AS order_count
FROM "order" AS o
WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
```

No cancellation predicate.

## RECIPE: QUALIFYING ORDER COUNT

```sql
SELECT
  COUNT(*) AS qualifying_order_count
FROM "order" AS o
WHERE NOT (
    lower(o.status::text) IN ('canceled', 'cancelled')
    OR upper(COALESCE(o.zoho_order_status, ''))
         IN ('CANCELED', 'CANCELLED')
  )
  AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
```

## RECIPE: PERIOD COMPARISON

For requests such as:

`How many qualifying orders were placed in July 2026, and how does that compare with June 2026?`

do NOT return one combined scalar.

Preferred structure:

```sql
SELECT
  date_trunc(
    'month',
    timezone('Asia/Kolkata', o.created_at)
  )::date AS month,
  COUNT(*) AS qualifying_order_count
FROM "order" AS o
WHERE NOT (
    lower(o.status::text) IN ('canceled', 'cancelled')
    OR upper(COALESCE(o.zoho_order_status, ''))
         IN ('CANCELED', 'CANCELLED')
  )
  AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
GROUP BY month
ORDER BY month
```

Expected parameters can cover both requested periods, for example:

```text
$1 = 2026-06-01
$2 = 2026-08-01
```

Result shape:

comparison/time series

not scalar.

## RECIPE: WEEKLY ORDER VOLUME

For:

`Compare week-by-week placed order volume from January through February 2026`

use placed-order population.

Do NOT add qualifying cancellation logic.

Conceptual query:

```sql
SELECT
  date_trunc(
    'week',
    timezone('Asia/Kolkata', o.created_at)
  )::date AS week,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
GROUP BY week
ORDER BY week
```

Important:

A week bucket may begin before or after the requested calendar-month labels depending on how partial weeks intersect the period.

The SQL WHERE boundaries must still remain exactly the requested period.

Do not allow rows outside the requested end boundary merely because the week label is later.

## RECIPE: MONTHLY ORDER VOLUME

```sql
SELECT
  date_trunc(
    'month',
    timezone('Asia/Kolkata', o.created_at)
  )::date AS month,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
  AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
GROUP BY month
ORDER BY month
```

Add `QUALIFYING_ORDER` only when requested.

## RECIPE: OPERATIONAL STATUS BREAKDOWN

```sql
SELECT
  o.zoho_order_status AS operational_status,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE <TIME_RANGE>
GROUP BY o.zoho_order_status
ORDER BY order_count DESC
```

Preserve NULL as unknown.

Do not use native `o.status` unless the request explicitly asks for native order status.

## RECIPE: PAYMENT STATUS BREAKDOWN

```sql
SELECT
  o.payment_status,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE <TIME_RANGE>
GROUP BY o.payment_status
ORDER BY order_count DESC
```

Use plain placed or qualifying population according to the request.

## RECIPE: STORE BREAKDOWN

Conceptual:

```sql
SELECT
  o.store_id,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE <TIME_RANGE>
GROUP BY o.store_id
ORDER BY order_count DESC
```

Load store metadata only if it is supplied and required for display.

Do not invent store-name relationships.

## RECIPE: B2B VS NON-B2B

Conceptual:

```sql
SELECT
  CASE
    WHEN o.is_b2b IS TRUE THEN 'B2B'
    ELSE 'NON_B2B'
  END AS order_type,
  COUNT(*) AS order_count
FROM "order" AS o
WHERE <TIME_RANGE>
GROUP BY order_type
ORDER BY order_type
```

Use a different NULL treatment only if the supplied business rule requires it.

## ORDER LIST REQUESTS

Examples:

* `Show orders from yesterday`
* `List qualifying orders from July`
* `Which orders are fulfilled?`

Return only required safe fields.

Typical minimum fields:

```text
order_id
created_at
status
zoho_order_status
payment_status
fulfillment_status
store_id
is_b2b
```

Do not use:

```sql
SELECT *
```

Do not retrieve customer PII unless an explicit authorized customer-domain request requires it.

## RESULT SHAPE

### Scalar

Examples:

`How many orders yesterday?`

`How many qualifying orders in July?`

### Time series / comparison

Examples:

`How many orders in July compared with June?`

`Orders week by week`

`Orders each month`

### Grouped

Examples:

`Orders by store`

`Orders by operational status`

`Orders by payment status`

### Ranking

Examples:

`Top stores by order count`

Comparison/grouping/ranking intent overrides the presence of:

* how many
* count
* number of

Do not collapse requested groups into one scalar.

## CHILD TABLE SAFETY

Known one-to-many order children include domains such as:

* line_item
* payment
* fulfillment
* refund/cancellation structures

If a question only needs order columns:

do not join child tables.

For example:

`total orders vs fulfilled orders on May 7`

requires only:

```text
"order"
```

Do not join fulfillment merely because the word `fulfilled` appears when `OPERATIONALLY_FULFILLED` is defined from `o.zoho_order_status`.

## PLACED VS QUALIFYING

This distinction is mandatory.

### Plain request

`orders`

→ `PLACED_ORDER`

### Explicit request

`qualifying orders`

→ `QUALIFYING_ORDER`

### Explicit request

`non-cancelled orders`

→ `QUALIFYING_ORDER`

Do not assume sales-oriented cancellation exclusion unless the wording or selected metric explicitly requires it.

## STATUS DISTINCTIONS

Keep these separate:

```text
NATIVE_ORDER_STATUS
= o.status
```

```text
OPERATIONAL_STATUS
= o.zoho_order_status
```

```text
PAYMENT_STATUS
= o.payment_status
```

```text
NATIVE_FULFILLMENT_STATUS
= o.fulfillment_status
```

```text
ACTUAL_DELIVERY_STATUS
= fulfillment.delivery_status
```

Do not substitute one for another.

## DO NOT USE

Do NOT:

* use `o.updated_at` as order date
* use `o.canceled_at` as cancellation truth
* use `o.delivery_status` as actual delivery truth
* exclude cancelled orders from plain placed-order volume
* include fully cancelled orders in explicit qualifying population
* treat partial cancellation as full cancellation
* use native status when operational status was requested
* use native fulfillment status when operationally fulfilled was requested
* add child-table joins when order columns alone answer the question
* use `SELECT *`
* collapse comparisons into scalar results
* use one-sided bounds for exact dates/months

## QUERY CONSTRUCTION CHECKLIST

For every order query determine:

```text
1. placed or qualifying?
2. exact time boundaries?
3. scalar, comparison, time series, grouped, or ranking?
4. native status or operational status?
5. fulfilled = operational or explicitly native?
6. payment status only, or money value too?
7. store/B2B segmentation requested?
8. can the query be answered from "order" alone?
9. are any child joins required?
10. if child joins exist, is order grain protected?
```

Then use the smallest correct SQL structure.

If a required order concept is not defined here or in another selected domain pack, fail planning rather than guess.