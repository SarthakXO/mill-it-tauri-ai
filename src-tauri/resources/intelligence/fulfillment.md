# Fulfillment domain

Use this domain for packing, shipping, delivery-state analysis, fulfillment lifecycle timing, fulfillment backlogs, and order-level delivery summaries.

Use verified fulfillment tables/columns only.

Do not use `order.delivery_status` as actual delivery truth.

## TABLE: fulfillment

Grain:

one row per fulfillment

Alias:

`f`

An order may have multiple fulfillment rows.

Therefore:

```text
fulfillment.id
= fulfillment grain

order.id
= order grain
````

Do not assume one fulfillment per order.

## APPROVED COLUMNS

### f.id

Meaning:

canonical fulfillment identity

Use for:

* fulfillment-grain counts
* fulfillment record identity

### f.order_id

Meaning:

relationship from fulfillment to order

Relationship:

```text
fulfillment.order_id
many → one
order.id
```

Use this to aggregate fulfillment activity to order grain.

### f.packed_at

Meaning:

packing timestamp

Use for:

* packed-state derivation
* packing timestamps
* packing duration
* packed backlog

Canonical packed condition:

```sql
f.packed_at IS NOT NULL
```

### f.shipped_at

Meaning:

shipping timestamp

Use for:

* shipped-state derivation
* shipping timestamps
* packing-to-shipping duration
* shipped backlog

Canonical shipped condition:

```sql
f.shipped_at IS NOT NULL
```

### f.delivery_status

Meaning:

actual custom fulfillment/delivery/rider state

Use for:

* actual delivery-state analysis
* delivery-status breakdown
* delivered/not-delivered operational analysis

This is the canonical source for actual delivery state.

Preserve `NULL` as unknown.

Do not silently convert `NULL` to:

* pending
* undelivered
* failed
* delivered

unless a verified domain rule explicitly defines that mapping.

## TABLE: "order"

Relevant fulfillment-related columns:

### o.id

Meaning:

canonical order identity

Use when fulfillment results are requested at order grain.

### o.fulfillment_status

Meaning:

native Medusa aggregate fulfillment lifecycle

Use only when the request explicitly concerns native fulfillment state.

This is NOT the same as actual custom delivery state.

### o.zoho_order_status

Meaning:

Mill-it operational order status

Use for operational completion concepts.

Canonical operational fulfillment/completion:

```sql
upper(COALESCE(o.zoho_order_status, '')) = 'FULFILLED'
```

### o.delivery_status

Do NOT use for actual delivery analysis.

The discovered data does not make this field authoritative for real delivery state.

## RELATIONSHIPS

```text
"order".id
one → many
fulfillment.order_id
```

An order may have multiple fulfillment rows.

Therefore order-level fulfillment questions require preaggregation or deterministic reduction to order grain.

## CONCEPT: NATIVE_FULFILLMENT_STATUS

Source:

```text
order.fulfillment_status
```

Use when the user explicitly asks for:

* native fulfillment status
* Medusa fulfillment status
* fulfillment_status breakdown

Do not treat this as actual delivery state.

## CONCEPT: OPERATIONALLY_FULFILLED

Source:

```text
order.zoho_order_status
```

Canonical predicate:

```sql
upper(COALESCE(o.zoho_order_status, '')) = 'FULFILLED'
```

Use for questions such as:

* fulfilled orders
* operationally fulfilled orders
* operational completion

unless the user explicitly asks for native fulfillment status instead.

Example:

`How many fulfilled orders were there on May 7?`

Use:

```text
OPERATIONALLY_FULFILLED
```

not:

```text
order.fulfillment_status
```

unless the request explicitly says native/Medusa fulfillment.

## CONCEPT: PACKED

Canonical source:

```text
fulfillment.packed_at
```

Canonical condition:

```sql
f.packed_at IS NOT NULL
```

For order-level packed status, aggregate fulfillment rows to order grain.

Do not count fulfillment rows directly if the question asks for orders.

## CONCEPT: SHIPPED

Canonical source:

```text
fulfillment.shipped_at
```

Canonical condition:

```sql
f.shipped_at IS NOT NULL
```

For order-level shipped status, aggregate fulfillment rows to order grain.

## CONCEPT: ACTUAL_DELIVERY_STATUS

Canonical source:

```text
fulfillment.delivery_status
```

Use for:

* actual delivery state
* delivery-status breakdown
* delivered-state analysis

Do NOT use:

```text
order.delivery_status
```

as actual delivery truth.

## ORDER-GRAIN PREAGGREGATION

For order-level fulfillment analysis, reduce fulfillment rows before joining to other order children.

Example pattern:

```sql
WITH fulfillment_by_order AS (
  SELECT
    f.order_id,
    BOOL_OR(f.packed_at IS NOT NULL) AS is_packed,
    BOOL_OR(f.shipped_at IS NOT NULL) AS is_shipped
  FROM fulfillment AS f
  GROUP BY f.order_id
)
SELECT ...
FROM "order" AS o
LEFT JOIN fulfillment_by_order AS fb
  ON fb.order_id = o.id
```

Use equivalent deterministic aggregation supported by the actual query.

Do not raw-join fulfillment to line items/payments and then aggregate order counts or money.

## MULTIPLE FULFILLMENTS

An order may have multiple fulfillment rows.

Do not arbitrarily choose:

```text
first fulfillment
latest fulfillment
MAX(delivery_status)
MIN(delivery_status)
```

unless a verified business rule defines that reduction.

For boolean lifecycle questions such as:

* ever packed
* ever shipped

safe order-level aggregation may use:

```sql
BOOL_OR(...)
```

For delivery status, use a documented status-reduction rule if one exists.

If no authoritative order-level reduction rule exists, preserve fulfillment grain or return a controlled limitation rather than inventing one.

## RECIPE: PACKED ORDERS

Conceptual order-grain structure:

```sql
WITH fulfillment_by_order AS (
  SELECT
    f.order_id,
    BOOL_OR(f.packed_at IS NOT NULL) AS is_packed
  FROM fulfillment AS f
  GROUP BY f.order_id
)
SELECT
  COUNT(*) AS packed_order_count
FROM "order" AS o
JOIN fulfillment_by_order AS fb
  ON fb.order_id = o.id
WHERE fb.is_packed
  AND <ORDER_TIME_RANGE>
```

Use placed or qualifying order population according to the user's wording.

Do not automatically add qualifying-order semantics to a plain operational count unless requested.

## RECIPE: PACKED BUT NOT SHIPPED

Order-level concept:

```text
at least one packed fulfillment
AND
no shipped fulfillment
```

Possible order-grain aggregation:

```sql
WITH fulfillment_by_order AS (
  SELECT
    f.order_id,
    BOOL_OR(f.packed_at IS NOT NULL) AS is_packed,
    BOOL_OR(f.shipped_at IS NOT NULL) AS is_shipped
  FROM fulfillment AS f
  GROUP BY f.order_id
)
SELECT ...
FROM "order" AS o
JOIN fulfillment_by_order AS fb
  ON fb.order_id = o.id
WHERE fb.is_packed
  AND NOT fb.is_shipped
```

Use the requested order population/time window separately.

## RECIPE: SHIPPED ORDERS

Order-level concept:

```text
at least one fulfillment with shipped_at IS NOT NULL
```

Aggregate to order grain before counting orders.

## RECIPE: DELIVERY STATUS BREAKDOWN

If the user asks for fulfillment-level delivery statuses:

group by:

```text
f.delivery_status
```

Preserve NULL as unknown.

If the user asks for delivery status by order:

first determine whether a verified order-level delivery-state reduction rule is available.

Do not choose an arbitrary fulfillment row.

If no valid reduction exists, return fulfillment-grain status or a controlled limitation.

## LIFECYCLE DURATION

Canonical lifecycle timestamps:

```text
packed_at
shipped_at
```

Example:

`How long from packing to shipping?`

Use only rows where:

```sql
f.packed_at IS NOT NULL
AND f.shipped_at IS NOT NULL
```

Duration:

```sql
f.shipped_at - f.packed_at
```

Do not invent missing lifecycle timestamps.

## TIMESTAMP SEQUENCE SAFETY

Observed operational data may contain anomalies.

Do not assume every fulfillment follows:

```text
packed
→ shipped
→ delivered
```

perfectly.

For duration calculations, use only rows with the required timestamps and valid ordering when required.

Example:

```sql
f.shipped_at >= f.packed_at
```

when calculating packing-to-shipping duration.

Do not fabricate intermediate stages.

## BACKLOG CONCEPTS

Backlog definitions must be explicit.

Examples:

### PACKING_BACKLOG

Possible meaning:

orders in the requested operational population that have not yet been packed.

Do not invent the qualifying population.

Use the current question's requested status/population.

### SHIPPING_BACKLOG

Possible meaning:

packed orders not yet shipped.

Canonical lifecycle inputs:

```text
packed_at
shipped_at
```

### DELIVERY_BACKLOG

Requires an explicit supported interpretation of `fulfillment.delivery_status`.

Do not infer a universal backlog definition from NULL alone.

## OPERATIONAL FULFILLMENT VS DELIVERY

Keep these distinct:

```text
OPERATIONALLY_FULFILLED
= order.zoho_order_status = FULFILLED
```

```text
NATIVE_FULFILLMENT_STATUS
= order.fulfillment_status
```

```text
ACTUAL_DELIVERY_STATUS
= fulfillment.delivery_status
```

These are separate concepts.

Do not substitute one for another.

## RESULT SHAPE

Examples:

`How many orders were fulfilled yesterday?`

→ scalar

`Compare fulfilled orders in June vs July`

→ comparison/time series

`Fulfilled orders by store`

→ grouped result

`Delivery-status breakdown`

→ grouped result

`Packing-to-shipping time by week`

→ time series/grouped aggregate

Comparison/grouping overrides `how many`.

## JOIN SAFETY

Fulfillment is a one-to-many child of order.

Do not generate:

```text
order
JOIN fulfillment
JOIN line_item
```

and then directly aggregate:

* order count
* units
* money

without reducing the relevant child tables first.

Preferred pattern:

```text
fulfillment
→ aggregate to order grain

line_item
→ aggregate to order grain

then join to order
```

## PII / SENSITIVE FIELDS

Generic fulfillment analytics should not retrieve:

* customer names
* customer phone
* customer email
* addresses
* exact delivery coordinates
* rider/staff personal details
* tracking/proof data
* delivery images

unless a separate explicit authorized local-record contract permits it.

Do not send fulfillment-related PII to Qwen analysis.

## DO NOT USE

Do NOT:

* use `order.delivery_status` as actual delivery truth
* equate `order.fulfillment_status` with delivery status
* equate operationally fulfilled with native fulfillment status
* assume one fulfillment per order
* arbitrarily choose one fulfillment status from multiple rows
* count fulfillment rows when the question asks for order count
* raw-join multiple one-to-many child tables before aggregation
* invent missing packed/shipped/delivery timestamps
* convert NULL delivery status into another status without a rule

## QUERY CONSTRUCTION CHECKLIST

For a fulfillment query determine:

```text
1. order grain or fulfillment grain?
2. native fulfillment, operational fulfillment, packing, shipping, or delivery?
3. placed or qualifying order population?
4. time range based on order date or lifecycle timestamp?
5. multiple fulfillment rows possible?
6. must fulfillment be preaggregated to order grain?
7. lifecycle duration requested?
8. grouping/comparison requested?
```

Then compose the query from the canonical concepts above.

If the request requires an order-level delivery reduction rule that is not defined, fail planning rather than guess.