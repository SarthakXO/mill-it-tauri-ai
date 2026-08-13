# Products domain

Use this domain for product sales, variant/SKU analysis, units sold, net quantity, product/variant rankings, historical transaction labels, and product-demand inputs.

Product analytics must preserve the requested grain:

```text
product
!=
variant
!=
line item
````

## TABLE: line_item

Grain:

one historical order line

Alias:

`li`

## APPROVED COLUMNS

### li.id

Meaning:

canonical line-item identity

Use for:

* line-level grain
* partial-cancellation linkage
* deterministic line aggregation

### li.order_id

Meaning:

relationship to order

Relationship:

```text
line_item.order_id
many → one
order.id
```

Use for joining transaction lines to order population/date semantics.

### li.variant_id

Meaning:

historical analytical variant identity

Canonical variant key:

```text
li.variant_id
```

Use for:

* variant sales
* SKU-style analytics
* reorder history
* variant demand
* variant ranking

Do not replace this with literal `product_variant.sku`.

### li.title

Meaning:

historical transaction display label

Use for:

* readable historical display
* representative labels

Do NOT use as analytical identity.

Do NOT group by:

```sql
li.variant_id, li.title
```

when variant grain is requested.

One variant may have different historical titles over time.

### li.quantity

Meaning:

original ordered line quantity

Important:

this is not automatically adjusted when part of the line is cancelled.

For net sales/demand, use `NET_LINE_QUANTITY`.

### li.unit_price

Meaning:

historical transaction unit price

Use only when a money-domain metric explicitly requires historical line value.

Do not replace with current catalog pricing.

## TABLE: product_variant

Alias:

`pv`

Grain:

one current product variant

Relevant relationship:

```text
line_item.variant_id
many → one
product_variant.id
```

Historical lines may remain valid even if current catalog enrichment is missing.

Use LEFT JOIN when current variant enrichment is optional.

## APPROVED COLUMNS

### pv.id

Meaning:

current variant identity

Equivalent analytical variant identity when matched to:

```text
li.variant_id
```

### pv.product_id

Meaning:

roll-up relationship from variant to product

Canonical product-grain key:

```text
pv.product_id
```

Use when the user asks for:

* products
* product ranking
* product demand
* product-level sales

### pv.title

Meaning:

current variant display title

Use only as optional current display metadata.

Do not use as historical transaction identity.

### pv.sku

Meaning:

literal stored SKU field

This field is not the normal analytical variant identity.

Use only when the user explicitly asks for:

* literal SKU
* SKU string
* stored SKU code

Otherwise:

```text
SKU-style analytical identity
→ variant_id
```

## TABLE: product

Alias:

`p`

Grain:

one current product

Relevant relationship:

```text
product_variant.product_id
many → one
product.id
```

## APPROVED COLUMNS

### p.id

Meaning:

canonical product identity

Use for product-grain grouping when current product enrichment exists.

### p.title

Meaning:

current product display title

Use only as optional display metadata.

Do not require current product existence for historical transaction inclusion.

## PARTIAL CANCELLATION SOURCE

Use the verified partial-cancellation table/relationship supplied by the active physical schema.

The required semantic relationship is:

```text
partial cancellation
→ line_item.id
```

Preaggregate cancelled quantity by line item before joining to transaction lines.

Do not raw-join multiple partial-cancellation rows and then sum line quantity.

## CONCEPT: VARIANT_GRAIN

Definition:

one analytical row per:

```text
variant_id
```

Canonical key:

```text
li.variant_id
```

Use for:

* top variants
* variant sales
* SKU-level analytics
* pack-size analysis
* variant forecast history

Do not add display labels to the grouping key.

Correct:

```sql
GROUP BY li.variant_id
```

Incorrect:

```sql
GROUP BY li.variant_id, li.title
```

## CONCEPT: PRODUCT_GRAIN

Definition:

one analytical row per product, combining all variants belonging to that product.

Canonical roll-up key:

```text
pv.product_id
```

Typical relationship:

```text
line_item.variant_id
→ product_variant.id
→ product_variant.product_id
```

Use when the user asks for:

* products
* best-selling products
* product demand
* product ranking

Do NOT answer a product request at variant grain.

## PRODUCT VS VARIANT WORDING

Interpret literally.

Examples:

```text
top products
→ PRODUCT_GRAIN
```

```text
top variants
→ VARIANT_GRAIN
```

```text
top SKUs
→ VARIANT_GRAIN
```

unless the user explicitly requests the literal stored SKU string.

```text
pack sizes
→ usually VARIANT_GRAIN
```

Do not silently substitute one grain for another.

## CONCEPT: NET_LINE_QUANTITY

Definition:

original line quantity reduced by verified partial-cancelled quantity.

Canonical expression:

```sql
GREATEST(
  COALESCE(li.quantity, 0)::bigint
  - COALESCE(pc.partial_cancelled_qty, 0),
  0
)
```

where:

```text
pc
```

is a line-item-grain partial-cancellation aggregate.

For qualifying sales/demand:

* exclude fully cancelled orders using `QUALIFYING_ORDER`;
* subtract partial-cancelled quantity;
* never allow quantity below zero.

## RECIPE: PARTIAL CANCELLATION AGGREGATE

Conceptual structure:

```sql
WITH partial_cancel_by_line AS (
  SELECT
    <line_item_id> AS line_item_id,
    SUM(<cancelled_quantity>)::bigint AS partial_cancelled_qty
  FROM <verified_partial_cancellation_source>
  GROUP BY <line_item_id>
)
```

Use the exact verified physical table and columns supplied by the schema.

Do not invent column names.

## CONCEPT: NET_UNITS_SOLD

For qualifying product/variant sales:

```text
SUM(NET_LINE_QUANTITY)
```

over:

```text
QUALIFYING_ORDER
```

population.

Fully cancelled orders contribute no units.

Partial cancellation subtracts only the cancelled quantity.

Do not use:

```sql
SUM(li.quantity)
```

when the request explicitly asks for:

* net units
* qualifying units
* units sold after cancellation
* best-selling products by net units

## RECIPE: VARIANT RANKING BY NET UNITS

For:

`Top 10 best-selling variants in July 2026 by net units sold`

use variant grain.

Conceptual structure:

```sql
WITH partial_cancel_by_line AS (
  SELECT
    <line_item_id> AS line_item_id,
    SUM(<cancelled_quantity>)::bigint AS partial_cancelled_qty
  FROM <verified_partial_cancellation_source>
  GROUP BY <line_item_id>
),
variant_sales AS (
  SELECT
    li.variant_id,
    SUM(
      GREATEST(
        COALESCE(li.quantity, 0)::bigint
        - COALESCE(pc.partial_cancelled_qty, 0),
        0
      )
    ) AS net_units_sold
  FROM line_item AS li
  JOIN "order" AS o
    ON o.id = li.order_id
  LEFT JOIN partial_cancel_by_line AS pc
    ON pc.line_item_id = li.id
  WHERE <QUALIFYING_ORDER>
    AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
    AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
    AND li.variant_id IS NOT NULL
  GROUP BY li.variant_id
)
SELECT
  variant_id,
  net_units_sold
FROM variant_sales
ORDER BY net_units_sold DESC
LIMIT 10
```

Important:

group only by:

```text
variant_id
```

for the analytical metric.

Add labels separately.

## RECIPE: PRODUCT RANKING BY NET UNITS

For:

`Top 10 best-selling products in July 2026 by net units sold`

roll variant lines to product grain.

Conceptual structure:

```sql
WITH partial_cancel_by_line AS (
  SELECT
    <line_item_id> AS line_item_id,
    SUM(<cancelled_quantity>)::bigint AS partial_cancelled_qty
  FROM <verified_partial_cancellation_source>
  GROUP BY <line_item_id>
),
product_sales AS (
  SELECT
    pv.product_id,
    SUM(
      GREATEST(
        COALESCE(li.quantity, 0)::bigint
        - COALESCE(pc.partial_cancelled_qty, 0),
        0
      )
    ) AS net_units_sold
  FROM line_item AS li
  JOIN "order" AS o
    ON o.id = li.order_id
  LEFT JOIN partial_cancel_by_line AS pc
    ON pc.line_item_id = li.id
  LEFT JOIN product_variant AS pv
    ON pv.id = li.variant_id
  WHERE <QUALIFYING_ORDER>
    AND timezone('Asia/Kolkata', o.created_at) >= (($1::text)::date)
    AND timezone('Asia/Kolkata', o.created_at) < (($2::text)::date)
    AND pv.product_id IS NOT NULL
  GROUP BY pv.product_id
)
SELECT
  product_id,
  net_units_sold
FROM product_sales
ORDER BY net_units_sold DESC
LIMIT 10
```

Do not group by:

```text
variant_id
```

when the request is product grain.

## HISTORICAL LABELS

Historical analytical identity is ID-based.

Display labels should be selected separately.

For variant results:

preferred historical label source:

```text
line_item.title
```

Choose one representative title without changing the metric grain.

A useful deterministic rule is:

```text
most recent non-null transaction title within the requested period
```

Conceptual pattern:

```sql
ROW_NUMBER() OVER (
  PARTITION BY li.variant_id
  ORDER BY o.created_at DESC, li.id DESC
)
```

then choose:

```text
rn = 1
```

as display metadata.

Do not aggregate the metric by title.

## RECIPE: VARIANT RANKING WITH HISTORICAL LABEL

Conceptually separate:

```text
metric aggregation
```

from:

```text
label selection
```

Example structure:

```sql
WITH
partial_cancel_by_line AS (...),

variant_sales AS (
  SELECT
    li.variant_id,
    SUM(<NET_LINE_QUANTITY>) AS net_units_sold
  FROM ...
  GROUP BY li.variant_id
),

variant_labels AS (
  SELECT
    variant_id,
    title
  FROM (
    SELECT
      li.variant_id,
      li.title,
      ROW_NUMBER() OVER (
        PARTITION BY li.variant_id
        ORDER BY o.created_at DESC, li.id DESC
      ) AS rn
    FROM line_item AS li
    JOIN "order" AS o
      ON o.id = li.order_id
    WHERE <SAME_TIME_RANGE>
      AND li.variant_id IS NOT NULL
  ) x
  WHERE rn = 1
)

SELECT
  vs.variant_id,
  vl.title,
  vs.net_units_sold
FROM variant_sales AS vs
LEFT JOIN variant_labels AS vl
  ON vl.variant_id = vs.variant_id
ORDER BY vs.net_units_sold DESC
LIMIT 10
```

The exact query may combine these stages differently.

The required rule is:

```text
label must not define metric grain
```

## PRODUCT LABELS

Product analytical identity:

```text
product_id
```

Current `product.title` may be used as optional display enrichment.

Use:

```text
LEFT JOIN product
```

when current catalog enrichment is optional.

Do not discard historical product sales merely because the current product row is unavailable or soft deleted.

If no current product title is available:

return product ID and another safe historical label if a verified derivation exists.

Do not invent a title.

## CURRENT CATALOG VS HISTORICAL TRANSACTIONS

Historical transaction facts come from:

```text
line_item
```

plus order/cancellation semantics.

Current catalog tables are enrichment.

Therefore:

```text
historical line exists
+
current variant/product missing
```

must not automatically mean:

```text
drop transaction
```

Avoid current-catalog `INNER JOIN`s when they would remove valid historical facts, unless the requested metric fundamentally requires that current relationship and there is no historical mapping available.

## PRODUCT RANKING DEFAULTS

If the user asks:

`top products`

without specifying N:

default ranking limit may be:

```text
10
```

if this is the established application default.

If the user explicitly asks:

```text
top 5
top 20
```

preserve the requested limit.

Do not silently change the requested limit.

## RAW VS NET QUANTITY

Keep these distinct.

### RAW_ORDERED_QUANTITY

```text
SUM(li.quantity)
```

Use only when the user explicitly asks for:

* original ordered units
* gross ordered quantity
* pre-cancellation units

### NET_UNITS_SOLD

```text
SUM(NET_LINE_QUANTITY)
```

over qualifying orders.

Use for normal sales/demand ranking when the user asks for:

* net units
* best-selling
* units sold
* qualifying demand

unless another explicit definition is supplied.

## MULTI-PERIOD PRODUCT COMPARISON

For:

`Compare top products in June vs July`

preserve period as a dimension.

Do not aggregate the two months into one combined ranking unless explicitly requested.

Conceptual grain:

```text
period + product_id
```

For variants:

```text
period + variant_id
```

Comparison result must preserve each requested period.

## TIME FILTER

Product transaction period is based on:

```text
order.created_at
```

in IST unless another explicit event timestamp is requested.

Use:

```sql
timezone('Asia/Kolkata', o.created_at)
```

with:

```text
>= start
< end_exclusive
```

Do not filter product sales using:

* product.created_at
* variant.created_at
* line_item.updated_at

as the transaction date.

## ORDER POPULATION

For normal sales/demand concepts such as:

* best-selling products
* units sold
* net units sold
* product demand

use:

```text
QUALIFYING_ORDER
```

unless the user explicitly requests raw placed-order demand.

For explicit:

`products ordered across all placed orders`

the population may be:

```text
PLACED_ORDER
```

Do not silently change requested population.

## PRODUCT FORECAST INPUT

When `forecasting.md` is selected:

### Variant forecast grain

```text
variant_id
```

### Product forecast grain

```text
product_id
```

Historical qualifying demand uses:

```text
NET_LINE_QUANTITY
```

Rust owns:

* zero-date filling
* forecast model
* forecast values
* forecast ranges
* future dates

Qwen only retrieves historical observations.

## REORDER INPUT

For customer reorder history:

canonical identity:

```text
customer_id + variant_id
```

Use historical qualifying orders.

Use:

```text
NET_LINE_QUANTITY
```

for historical quantity.

Do not roll reorder identity to product grain unless the specialized operation explicitly defines product-level reorder behavior.

## PRODUCT + MONEY JOIN SAFETY

Do not raw join:

```text
order
payment
line_item
```

then sum payment amount by product.

Payment is order-grain value and would repeat across lines.

If the user asks for product-level monetary attribution:

use only a verified allocation rule from `money.md`.

Do not invent proportional payment allocation.

## PRODUCT + FULFILLMENT JOIN SAFETY

If combining product metrics with fulfillment:

aggregate the required fulfillment concept to order grain first.

Do not allow fulfillment rows to multiply line quantities.

## RESULT SHAPE

Examples:

`Top 10 products in July`

→ ranking

`Net units by variant`

→ grouped result

`Products sold month by month`

→ time series/grouped

`Compare top products June vs July`

→ comparison/ranking

`How many units of variant X sold?`

→ scalar if no grouping/comparison requested

Preserve the requested product/variant grain.

## DO NOT USE

Do NOT:

* use `product_variant.sku` as normal analytical identity
* group variant metrics by `li.variant_id, li.title`
* group product metrics by variant ID
* answer product requests at variant grain
* answer variant requests at product grain
* use current titles as historical identity
* require current catalog existence for historical transaction facts
* use raw `li.quantity` for net-unit metrics
* treat partial cancellation as full order cancellation
* include fully cancelled orders in qualifying sales/demand
* let one-to-many joins multiply quantity
* use current prices to reconstruct historical transaction value
* invent product/variant labels

## QUERY CONSTRUCTION CHECKLIST

For every product query determine:

```text
1. product or variant grain?
2. raw quantity or net quantity?
3. placed or qualifying order population?
4. partial cancellation adjustment required?
5. transaction time range?
6. ranking/grouping/comparison requested?
7. limit requested?
8. historical label needed?
9. can label be selected separately from metric grain?
10. current catalog enrichment optional?
11. any money/fulfillment child joins that require preaggregation?
12. forecasting/reorder deterministic path required?
```

Then compose SQL from the verified concepts above.

If the physical schema does not supply a required product mapping, cancellation source, or relationship, fail planning rather than guess.