# Domain index

Use this file only to choose the smallest set of domain packs needed for the current request.

Do not use domain selection to redefine the user's metric.

## ORDERS

Use for:

- order count
- placed orders
- qualifying orders
- operational status
- native order status
- payment status
- fulfillment status at order level
- store
- B2B segmentation
- order lists
- date/day/week/month comparisons

Examples:

`How many orders were placed yesterday?`

→ `ORDERS`

`Compare qualifying orders in June and July`

→ `ORDERS`

`Total orders vs fulfilled orders on May 7`

→ `ORDERS`

`Orders by store`

→ `ORDERS`

## PRODUCTS

Use with `ORDERS` when the request involves:

- product sales
- variant sales
- SKU/variant analysis
- units sold
- net quantity
- product ranking
- variant ranking
- product performance
- historical product/variant labels

Examples:

`Top 10 products in July`

→ `ORDERS + PRODUCTS`

`Best-selling variants last month`

→ `ORDERS + PRODUCTS`

`Net units sold by product`

→ `ORDERS + PRODUCTS`

Do not select `PRODUCTS` for an ordinary order-count question.

## CUSTOMERS

Use with `ORDERS` when the request involves:

- unique customers
- repeat customers
- customer order history
- customer recency
- inactive/lapsed customers
- customer cohorts
- customer record lists
- customer purchase frequency

Examples:

`How many repeat customers ordered in July?`

→ `ORDERS + CUSTOMERS`

`Which customers have not ordered in 45 days?`

→ `ORDERS + CUSTOMERS`

`Customers with at least 5 orders in 6 months`

→ `ORDERS + CUSTOMERS`

## MONEY

Use with `ORDERS` when the request involves monetary value such as:

- AOV
- order value
- qualifying order value
- placed order value
- captured value
- refunds
- net collected / net realized operational value
- monetary comparisons

Examples:

`Qualifying AOV by month`

→ `ORDERS + MONEY`

`Refund amount this month`

→ `ORDERS + MONEY`

`Captured value by payment status`

→ `ORDERS + MONEY`

Do not select `MONEY` merely because the question mentions payment status without monetary value.

Example:

`How many orders are awaiting payment?`

→ `ORDERS`

## FULFILLMENT

Use with `ORDERS` when the request involves:

- packing
- shipping
- fulfillment operations
- actual delivery/rider state
- fulfillment lifecycle timestamps
- packing/shipping backlog
- order-level delivery breakdown

Examples:

`Show delivery-status breakdown`

→ `ORDERS + FULFILLMENT`

`Packed but not shipped orders`

→ `ORDERS + FULFILLMENT`

`How long does packing to shipping take?`

→ `ORDERS + FULFILLMENT`

Do not use the fulfillment domain merely because `order.fulfillment_status` is requested; basic order-level native fulfillment-status queries may be handled by `ORDERS`.

## FORECASTING

Use when the user requests a future prediction or deterministic reorder prediction.

Examples:

- forecast
- predict
- expected future orders
- future demand
- next N days
- likely due to reorder
- expected reorder date
- reorder demand
- overdue reorder

### Order forecast

`Forecast orders for the next 14 days`

→ `ORDERS + FORECASTING`

### Product/variant forecast

`Forecast variant demand next month`

→ `ORDERS + PRODUCTS + FORECASTING`

### Reorder prediction

`Which customers are likely due to reorder?`

→ `ORDERS + PRODUCTS + CUSTOMERS + FORECASTING`

Forecast/reorder requests must route to deterministic Rust predictive functionality where supplied.

Do not use generated SQL itself to invent future predictions.

## Domain combinations

Common combinations:

```text
Order volume
→ ORDERS

Order comparison
→ ORDERS

Product ranking
→ ORDERS + PRODUCTS

Variant ranking
→ ORDERS + PRODUCTS

Customer cohort
→ ORDERS + CUSTOMERS

Customer-product history
→ ORDERS + PRODUCTS + CUSTOMERS

AOV
→ ORDERS + MONEY

Delivery
→ ORDERS + FULFILLMENT

Order forecast
→ ORDERS + FORECASTING

Product forecast
→ ORDERS + PRODUCTS + FORECASTING

Reorder prediction
→ ORDERS + PRODUCTS + CUSTOMERS + FORECASTING
````

## Selection rules

Select the minimum sufficient domain set.

Do not add unrelated packs.

Examples:

`How many orders yesterday?`

Do NOT load:

* products
* customers
* money
* fulfillment
* forecasting

unless required by another explicit part of the question.

`Top products`

requires products because product grain and quantity semantics live there.

`AOV`

requires money because monetary definitions live there.

`Delivery status`

requires fulfillment because actual delivery state lives there.

## Metric meaning is defined by domain packs

Domain selection does not itself define a metric.

Examples:

`orders`

does not automatically mean:

`qualifying orders`

`products`

does not automatically mean:

`variants`

`fulfilled`

uses the canonical fulfilled concept supplied by the selected order/fulfillment context.

`AOV`

uses the canonical AOV definition supplied by `MONEY`.

`delivery status`

must use the delivery concept supplied by `FULFILLMENT`.

`SKU`

uses analytical variant identity unless the user explicitly requests the literal stored SKU field.

## Result shape is independent of domain selection

Examples:

`How many orders yesterday?`

```text
domains = ORDERS
shape = scalar
```

`How many orders each month?`

```text
domains = ORDERS
shape = time_series
```

`How many orders by store?`

```text
domains = ORDERS
shape = grouped
```

`Compare June vs July orders`

```text
domains = ORDERS
shape = comparison/time_series
```

`Top 10 products`

```text
domains = ORDERS + PRODUCTS
shape = ranking
```

Do not infer scalar shape merely because the request contains:

* how many
* count
* number of

Comparison, grouping, ranking, and time-series intent take precedence.

## Predictive routing

Descriptive questions:

```text
past/current database facts
→ SQL
```

Predictive questions:

```text
future forecast/reorder prediction
→ deterministic Rust operation
```

SQL may retrieve historical observations for a predictive operation.

Qwen must not generate the predictive values itself.

## Unsupported concepts

Do not treat these as fully supported unless a future domain definition explicitly enables them:

* accounting revenue
* recognized revenue
* GST-exclusive revenue
* tax-adjusted revenue
* authoritative historical B2B accounting value
* definitive COD cash settlement
* authoritative ingestion-completeness watermark
* actual delivery state from `order.delivery_status`
* literal stored SKU as normal analytical identity

If the user asks for an unsupported concept, select the relevant domain so it can explain/refuse the unsupported interpretation.

Do not invent a definition.