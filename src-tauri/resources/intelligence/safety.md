# Safety domain

Use this file for SQL execution safety, schema restrictions, privacy boundaries, result minimization, and semantic failure behavior.

Safety has two layers:

```text
1. Can this SQL safely execute?
2. Does this SQL still answer the question asked?
````

Both must pass.

## SQL EXECUTION CONTRACT

Generate exactly one read-only PostgreSQL query.

Allowed:

```sql
SELECT ...
```

or:

```sql
WITH ...
SELECT ...
```

CTEs are allowed only when the final statement is a `SELECT`.

## FORBIDDEN SQL

Never generate or execute:

```text
INSERT
UPDATE
DELETE
MERGE
UPSERT
TRUNCATE
DROP
ALTER
CREATE
GRANT
REVOKE
COPY
CALL
DO
```

Also forbid:

* transaction-control statements
* session-setting statements
* explicit locks
* sequence changes
* `SELECT INTO`
* dynamic SQL
* multiple SQL statements
* SQL comments
* file access
* network access
* program execution
* extension management
* unsafe functions outside the approved SQL capability

The query must remain read only.

## ONE STATEMENT

Exactly one SQL statement is allowed.

Valid:

```sql
WITH x AS (
  SELECT ...
)
SELECT ...
FROM x
```

Invalid:

```sql
SELECT ...;
SELECT ...;
```

Do not append diagnostic SQL or secondary statements.

## NO WILDCARD PROJECTIONS

Never use:

```sql
SELECT *
```

or:

```sql
table_alias.*
```

Select only explicitly required columns.

For record-list requests, return only fields needed by the UI/request.

## SCHEMA AUTHORITY

Use only:

* tables supplied by the active schema/context
* columns supplied by the active schema/context
* relationships supplied by the active domain packs
* verified PostgreSQL functions/operators

Do not invent:

* tables
* columns
* foreign keys
* enum/status values
* relationships
* views
* stored procedures
* business concepts

If the required schema information is absent:

fail planning rather than guess.

Physical/discovered schema and verified runtime behavior take precedence over stale ORM assumptions.

## PARAMETERS

User-controlled and resolved runtime values must use SQL placeholders.

Use contiguous placeholders:

```text
$1
$2
$3
```

Do not skip numbers.

Invalid:

```text
$1
$3
```

without `$2`.

Each placeholder must have exactly one corresponding scalar parameter.

Do not interpolate user text directly into SQL.

## PARAMETER TYPES

Cast placeholders explicitly when required.

Examples:

```sql
($1::text)::date
```

```sql
$2::bigint
```

Do not perform arithmetic on untyped or incompatible placeholder values.

Example of invalid conceptual arithmetic:

```text
TEXT - DATE
```

Use compatible PostgreSQL types.

## RESERVED RELATIONS

The order relation must always be quoted:

```sql
"order"
```

Do not generate:

```sql
FROM order
```

## BUSINESS TIME SAFETY

Business timezone:

```text
Asia/Kolkata
```

For order business-date semantics use the canonical domain expression:

```sql
timezone('Asia/Kolkata', o.created_at)
```

Exact bounded calendar periods require both:

```text
inclusive lower bound
exclusive upper bound
```

Canonical form:

```sql
event_time >= start
AND event_time < end_exclusive
```

Never answer an exact date/month/year using only a lower bound.

Example:

`yesterday`

must be:

```text
>= yesterday_start
< today_start
```

not:

```text
>= yesterday_start
```

The current IST day is incomplete unless an explicit current-day/nowcast path allows it.

If Rust supplies resolved date parameters, use those exact values.

Do not independently reinterpret resolved time boundaries.

## RESULT-SHAPE SAFETY

Preserve the analytical shape requested by the user.

Examples:

```text
single count
→ scalar
```

```text
June vs July
→ comparison
```

```text
each month
→ time series
```

```text
by store
→ grouped
```

```text
top 10
→ ranking
```

Words such as:

* how many
* count
* number of
* total

do not override stronger grouping/comparison/ranking intent.

Do not simplify:

```text
comparison
→ scalar
```

```text
grouping
→ scalar
```

```text
time series
→ scalar
```

```text
ranking
→ scalar
```

## SEMANTIC OBLIGATIONS

Explicit business concepts from the selected domain packs are mandatory.

Examples:

```text
QUALIFYING_ORDER
→ canonical cancellation exclusion
```

```text
PRODUCT_GRAIN
→ product_id roll-up
```

```text
VARIANT_GRAIN
→ variant_id
```

```text
NET_LINE_QUANTITY
→ original quantity minus partial cancellation
```

```text
OPERATIONALLY_FULFILLED
→ canonical operational status predicate
```

```text
QUALIFYING_AOV
→ canonical qualifying-order-value numerator and denominator
```

Do not replace a required concept with an easier approximation.

## PLACED VS QUALIFYING SAFETY

Keep populations distinct.

Plain:

```text
orders
orders placed
order volume
```

normally means:

```text
PLACED_ORDER
```

Do not automatically exclude cancelled orders.

Explicit:

```text
qualifying orders
non-cancelled orders
qualifying sales
```

means:

```text
QUALIFYING_ORDER
```

Do not silently convert one population into the other.

## CANCELLATION SAFETY

Canonical full cancellation is defined by the orders domain.

Do NOT use:

```text
order.canceled_at
```

as canonical cancellation truth.

Partial cancellation is separate from full order cancellation.

Do not exclude an entire order merely because one line/quantity was partially cancelled.

For net quantity/value metrics use the documented partial-cancellation adjustment.

## GRAIN SAFETY

Canonical analytical identity comes from IDs.

Examples:

```text
order
→ order.id
```

```text
customer
→ customer.id
```

```text
product
→ product.id / product_id
```

```text
variant
→ variant_id
```

```text
line
→ line_item.id
```

```text
fulfillment
→ fulfillment.id
```

Labels are display metadata.

Do not add display labels to `GROUP BY` if doing so changes analytical grain.

Invalid example for variant metrics:

```sql
GROUP BY li.variant_id, li.title
```

Correct analytical grain:

```sql
GROUP BY li.variant_id
```

Select a representative label separately.

## JOIN SAFETY

One-to-many relationships can multiply rows.

Known risky child datasets include:

* line items
* payments
* fulfillments
* partial cancellations
* refunds
* adjustments

Before joining multiple child datasets:

```text
child
→ aggregate/select to required parent grain
→ join
```

Examples:

```text
payment
→ one selected payment per order
```

```text
partial cancellation
→ one aggregate per line/order as required
```

```text
fulfillment
→ order-grain aggregate for order-level metrics
```

Never raw-join multiple many-side tables and then directly aggregate:

* order count
* money
* quantity
* customer count

## MONEY JOIN SAFETY

Do not generate:

```text
order
JOIN line_item
JOIN payment
```

then:

```sql
SUM(payment.amount)
```

because payment value will repeat across line rows.

Payment must first be selected to one applicable row/value per order.

Likewise preaggregate refunds/cancellations before joining them to payment/order values.

## PRODUCT JOIN SAFETY

Do not allow fulfillment/payment/cancellation joins to multiply line quantities.

For product or variant metrics:

```text
line metric
→ preserve line/variant/product grain
```

Aggregate unrelated child datasets before joining them.

## FULFILLMENT JOIN SAFETY

One order may have multiple fulfillment rows.

Do not count fulfillment rows when the question asks for order count.

For order-level fulfillment analysis:

```text
fulfillment
→ aggregate to order_id
→ join "order"
```

Do not arbitrarily choose one fulfillment row unless a verified reduction rule exists.

## CANONICAL SOURCE PROTECTIONS

Do not use convenient but incorrect sources.

### Cancellation

Do NOT use:

```text
order.canceled_at
```

Use canonical cancellation semantics from `orders.md`.

### Actual delivery

Do NOT use:

```text
order.delivery_status
```

Use:

```text
fulfillment.delivery_status
```

according to `fulfillment.md`.

### Captured monetary value

Do NOT use:

```text
payment.captured_at
```

as canonical captured-value truth.

Use the money-domain definition.

### Refunds

Do NOT use as canonical processed-refund truth:

```text
payment.amount_refunded
```

or native refund rows.

Use the verified refund-request workflow from `money.md`.

### Analytical SKU/variant identity

Do NOT normally use:

```text
product_variant.sku
```

Use:

```text
variant_id
```

unless the user explicitly requests literal SKU strings.

### Historical product identity

Do NOT use titles as identity.

Do not require current product/variant existence for historical transaction inclusion.

### Historical pricing

Do not reconstruct historical transaction value from current:

* product prices
* variant prices
* price lists
* B2B price lists

Use verified historical transaction/payment values.

## PRIVACY

Minimize PII.

Generic analytics must not retrieve or send to Qwen:

* customer name
* phone
* email
* date of birth
* address
* latitude/longitude
* rider identity
* staff identity
* tracking details
* delivery-proof details/images
* credentials
* secrets
* tokens
* OTP data
* password/password hashes

Canonical analytical customer identity:

```text
customer_id
```

is permitted.

## CUSTOMER LIST EXCEPTION

For an explicit authorized local customer-list request such as:

```text
Which customers have not ordered in 45 days?
```

the local UI may receive the minimum documented display fields from `customers.md`, for example:

```text
customer_id
first_name
phone
```

plus required analytical fields.

These PII-containing rows must NOT enter:

* Insight Analyst
* Qwen result analysis
* AnalysisPayload
* model-facing logs

Count-only customer queries must not retrieve names or phone numbers.

## RESULT MINIMIZATION

Return only data required by the question.

If the user asks:

```text
How many...
```

do not return individual records unless grouping/comparison requires them.

For potentially large record lists:

* use the requested/documented limit;
* select only required fields;
* avoid unnecessary child data.

## UNSUPPORTED BUSINESS TRUTH

Do not fabricate metrics whose verified definition does not exist.

Examples currently unsupported/caveated include:

* accounting revenue
* recognized revenue
* GST-exclusive revenue
* tax-exclusive revenue
* authoritative GST reporting
* unsupported historical B2B valuation
* definitive COD settlement
* causal explanations not demonstrated by data
* authoritative ingestion-completeness watermark

Use a clearly defined supported operational metric when appropriate.

Otherwise return a controlled limitation.

## FORECAST SAFETY

Qwen does not own predictive numerical truth.

Rust owns:

* forecast dates
* forecast values
* ranges
* backtests
* model selection
* reorder intervals
* reorder dates
* expected quantities
* confidence

Generated SQL may retrieve historical observations.

It must not fabricate predictions.

## ANALYST SAFETY

Insight Analyst receives only approved sanitized results/facts.

It may:

* summarize
* describe trends
* explain approved deterministic statistics
* suggest investigations

It must NOT:

* invent numbers
* alter numbers
* recalculate unsupported percentages
* invent forecasts
* invent confidence
* invent causes
* present correlation as causation

If the result already has a clear deterministic summary, analyst invocation is optional.

Do not invoke the analyst simply to restate a simple scalar or time series.

## REPAIR SAFETY

Repair may fix:

* SQL syntax
* incorrect placeholder usage
* missing required predicate
* missing exact upper/lower bound
* wrong grouping
* wrong grain
* incorrect canonical concept usage

Repair must preserve the original user request.

Do NOT repair by weakening semantics.

Forbidden repair examples:

```text
qualifying
→ placed
```

```text
comparison
→ scalar
```

```text
product
→ variant
```

```text
net units
→ raw quantity
```

```text
qualifying AOV
→ gross line average
```

```text
exact date range
→ lower-bound-only filter
```

If a faithful repair cannot be produced:

fail instead of executing a different query.

## SQL PARSEABILITY

Generated SQL must be valid PostgreSQL syntax.

Prefer simple, explicit structures.

Do not include pseudo-SQL placeholders such as:

```sql
WHERE <QUALIFYING_ORDER>
```

in final generated SQL.

Domain recipes use conceptual placeholders for documentation only.

The planner must replace them with actual supplied SQL definitions.

Do not output unresolved markers such as:

```text
<TIME_RANGE>
<NET_LINE_QUANTITY>
<verified_table>
...
```

## FINAL QUERY CHECKLIST

Before returning SQL verify:

```text
1. exactly one read-only SELECT?
2. valid PostgreSQL syntax?
3. no wildcard projection?
4. only supplied tables/columns?
5. contiguous placeholders?
6. correct time boundaries?
7. correct requested population?
8. correct analytical grain?
9. correct grouping/result shape?
10. required canonical concepts present?
11. one-to-many joins protected?
12. no unnecessary PII?
13. no unsupported metric substitution?
14. no unresolved conceptual placeholders?
```

If any required condition fails:

return a controlled planning failure rather than unsafe or semantically incorrect SQL.