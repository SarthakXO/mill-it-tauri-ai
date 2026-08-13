# Customers domain

Use this domain for customer counts, repeat behavior, inactivity/lapsed cohorts, qualifying purchase history, customer record lists, and reorder-history inputs.

Customer analytics must use the verified customer/order relationships and qualifying-order semantics supplied by the active context.

## TABLE: customer

Grain:

one row per customer

Alias:

`c`

### APPROVED COLUMNS

#### c.id

Meaning:

canonical customer identity

Use for:

- customer grouping
- customer counts
- joining from `order.customer_id`
- pseudonymous analytical identity

Do not replace customer identity with phone or email.

#### c.first_name

Meaning:

local-display customer first name

Use only for:

explicit authorized customer record-list requests

Examples:

- `which customers`
- `list customers`
- `show customers`

Do not retrieve for:

- aggregate counts
- repeat-rate calculations
- customer summaries
- model analysis

Must never enter Qwen analysis context or logs.

#### c.phone

Meaning:

local-display customer phone

Use only for:

explicit authorized customer record-list requests

Must never be used as canonical customer identity.

Must never enter Qwen analysis context or logs.

## TABLE: "order"

Relevant customer columns:

### o.id

Meaning:

canonical order identity

Use for:

qualifying order count

### o.customer_id

Meaning:

canonical order-to-customer relationship

Relationship:

```text
order.customer_id
many → one
customer.id
````

Use this field for customer purchase analytics.

### o.created_at

Meaning:

order creation / purchase timestamp

Business-date interpretation:

```sql
timezone('Asia/Kolkata', o.created_at)
```

Use qualifying purchases when calculating behavioral customer history.

## RELATIONSHIPS

```text
"order".customer_id
many → one
customer.id
```

Customer purchase behavior begins from qualifying orders.

For product/reorder history:

```text
customer.id
← order.customer_id
← line_item.order_id
→ line_item.variant_id
```

Use the relationships supplied by the active orders/products packs.

## CONCEPT: QUALIFYING_CUSTOMER_ORDER

Customer behavioral analytics use the canonical `QUALIFYING_ORDER` concept supplied by the orders/core context.

A fully cancelled order must NOT:

* increase customer qualifying-order count
* reset customer recency
* count as the customer's latest qualifying purchase
* contribute a reorder interval
* contribute reorder quantity

Do not use:

`order.canceled_at`

as cancellation truth.

## CONCEPT: UNIQUE_CUSTOMER

Definition:

one distinct canonical customer with at least one qualifying order in the requested population/window

Canonical identity:

`o.customer_id`

Typical SQL:

```sql
COUNT(DISTINCT o.customer_id)
```

Requirements:

* `o.customer_id IS NOT NULL`
* qualifying-order population where purchase behavior is requested

## CONCEPT: REPEAT_CUSTOMER

Definition:

customer with at least two qualifying orders in the relevant population/window

Canonical grouping:

```text
customer_id
```

Typical structure:

```sql
SELECT
  o.customer_id
FROM "order" AS o
WHERE <QUALIFYING_ORDER>
  AND <TIME_RANGE>
  AND o.customer_id IS NOT NULL
GROUP BY o.customer_id
HAVING COUNT(DISTINCT o.id) >= 2
```

Do not count cancelled orders toward repeat status.

## CONCEPT: LAST_QUALIFYING_ORDER

Definition:

latest qualifying order before the validated as-of / exclusive historical boundary

Use:

```sql
MAX(o.created_at)
```

or the IST business-date equivalent when a date is required.

This calculation must NOT be artificially limited by an unrelated eligibility-window lower bound.

## CONCEPT: INACTIVE_CUSTOMER

Definition:

customer whose latest qualifying order is older than the requested inactivity threshold.

The inactivity threshold must come from the current request or typed plan.

Do not invent a default inactivity threshold.

Example:

`inactive for 45 days`

means:

latest qualifying order is earlier than:

```text
as_of_date - 45 days
```

## CONCEPT: CUSTOMER_ELIGIBILITY_WINDOW

Some cohort requests have an eligibility condition inside a bounded historical window.

Example:

`customers with at least 5 orders in the last 6 months`

means:

count qualifying orders inside the six-month eligibility window.

This eligibility count is separate from customer recency.

## CONCEPT: ELIGIBILITY_PLUS_INACTIVITY

For requests such as:

`Which customers placed at least 5 qualifying orders in the last 6 months but have not placed a qualifying order in the last 45 days?`

perform TWO separate calculations.

### 1. Eligibility

Count qualifying orders inside the requested eligibility window.

### 2. Latest qualifying activity

Find the customer's latest qualifying order before the as-of boundary without restricting that calculation to the eligibility lower bound.

Do NOT calculate both values from the same bounded eligibility CTE if doing so would hide a more recent qualifying purchase.

Example:

Customer has:

* 5 qualifying orders earlier in the six-month window
* another qualifying order 20 days ago

Result:

customer is NOT inactive for 45 days.

The recent qualifying order must prevent inclusion.

## RECIPE: ELIGIBILITY + INACTIVITY COHORT

For a rolling-month eligibility window:

```sql
WITH eligible AS (
  SELECT
    o.customer_id,
    COUNT(DISTINCT o.id) AS order_count
  FROM "order" AS o
  WHERE <QUALIFYING_ORDER>
    AND o.customer_id IS NOT NULL
    AND timezone('Asia/Kolkata', o.created_at) >=
        (($1::text)::date - ($2::bigint * INTERVAL '1 month'))
    AND timezone('Asia/Kolkata', o.created_at) < (($1::text)::date)
  GROUP BY o.customer_id
  HAVING COUNT(DISTINCT o.id) >= $3
),
latest AS (
  SELECT
    o.customer_id,
    MAX(timezone('Asia/Kolkata', o.created_at)::date) AS last_order_date,
    MAX(o.created_at) AS last_order_at
  FROM "order" AS o
  WHERE <QUALIFYING_ORDER>
    AND o.customer_id IS NOT NULL
    AND timezone('Asia/Kolkata', o.created_at) < (($1::text)::date)
  GROUP BY o.customer_id
)
SELECT
  c.id AS customer_id,
  c.first_name,
  c.phone,
  eligible.order_count,
  latest.last_order_at,
  (($1::text)::date - latest.last_order_date) AS days_since_last_order
FROM eligible
JOIN latest
  ON latest.customer_id = eligible.customer_id
JOIN customer AS c
  ON c.id = eligible.customer_id
WHERE latest.last_order_date <
      (($1::text)::date - ($4::bigint * INTERVAL '1 day'))
ORDER BY days_since_last_order DESC
```

Parameter roles:

```text
$1 = validated as-of / exclusive historical boundary
$2 = eligibility window month count
$3 = minimum qualifying order count
$4 = inactivity threshold in days
```

Use contiguous placeholders.

Do not change placeholder roles without changing the scalar parameter array.

## ROLLING MONTHS VS COMPLETE MONTHS

These are different concepts.

### Rolling window

Phrases:

* `last 6 months`
* `past 6 months`

normally mean rolling calendar-month lookback from the validated as-of boundary unless the typed plan says otherwise.

Typical expression:

```sql
(($1::text)::date - ($2::bigint * INTERVAL '1 month'))
```

### Complete calendar months

Phrase:

`last 6 complete months`

means six fully completed calendar months.

Rust should resolve complete-calendar boundaries.

Do not convert rolling months into complete months or vice versa.

## DATE ARITHMETIC

Date differences must use compatible types.

Valid:

```sql
(($1::text)::date - latest.last_order_date)
```

Do NOT generate:

```text
TEXT - DATE
```

Do not perform arithmetic on uncast text values.

## CUSTOMER RESULT SHAPES

### Count-only request

Examples:

* `How many customers...`
* `How many repeat customers...`
* `How many inactive customers...`

Result:

aggregate/scalar unless comparison or grouping is also requested.

Do NOT retrieve:

* first_name
* phone
* other PII

for a count-only request.

### Explicit customer list

Examples:

* `Which customers...`
* `List customers...`
* `Show customers...`

Result:

local customer record list.

Allowed minimum fields when needed:

```text
customer_id
first_name
phone
```

plus requested safe cohort fields such as:

```text
order_count
last_order_at
days_since_last_order
```

Do not use:

`SELECT *`

Do not retrieve the full customer row.

## COMPARISON / GROUPING

Comparison and grouping override `how many`.

Examples:

`How many repeat customers each month?`

→ time series

`How many inactive customers by store?`

→ grouped result

`Compare repeat customers in June vs July`

→ comparison

Do not collapse these into one scalar.

## REORDER HISTORY

Reorder calculations are deterministic Rust functionality.

SQL supplies historical purchase observations.

Canonical reorder identity:

```text
customer_id + variant_id
```

Historical sequence uses qualifying purchases ordered by qualifying purchase date.

Useful source fields include:

```text
customer_id
variant_id
qualifying purchase date
net quantity
```

Do not use:

* customer phone
* customer email
* product labels

as reorder identity.

## REORDER QUANTITY

When reorder quantity is required, use the `NET_LINE_QUANTITY` concept from the products domain.

Do not use raw `line_item.quantity` if partial cancellations are relevant.

Fully cancelled orders contribute no reorder purchase observation.

## REORDER STATISTICS

Rust owns calculation of:

* reorder intervals
* average interval
* median interval
* last purchase
* expected reorder date
* overdue days
* average historical quantity
* median historical quantity
* confidence score
* confidence label

If the user asks for:

`average historical quantity`

return the average.

Do not silently substitute median and call it average.

Qwen does not generate predictive reorder numbers.

## PRIVACY

Canonical analytical customer identity:

`customer.id`

Never identify or deduplicate customers using:

* phone
* email
* name

Customer PII must not enter:

* Qwen analysis context
* Insight Analyst payload
* model-facing logs

Explicit authorized local customer lists may display the minimum permitted PII directly in the UI.

Customer list rows containing PII should bypass the Insight Analyst.

## DO NOT USE

Do NOT:

* count fully cancelled orders as customer purchases
* use `order.canceled_at` for cancellation truth
* use phone/email as customer identity
* calculate last qualifying order only inside an eligibility lower bound
* retrieve PII for aggregate-only questions
* use `SELECT *`
* pass customer PII to Qwen
* let Qwen calculate reorder predictions

## QUERY CONSTRUCTION CHECKLIST

For a customer query determine:

```text
1. count or list?
2. qualifying purchase behavior?
3. eligibility window?
4. inactivity/recency requirement?
5. does latest activity need a separate CTE?
6. grouping/comparison requested?
7. is local-display PII explicitly required?
8. is this descriptive SQL or deterministic reorder prediction?
```

Then compose SQL using the concepts and recipes above.

If the request requires customer semantics not defined here or in another selected domain pack, fail planning rather than guess.