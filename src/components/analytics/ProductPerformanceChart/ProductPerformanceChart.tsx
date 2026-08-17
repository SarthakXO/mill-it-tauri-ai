import { useEffect, useMemo, useState } from "react";
import type { ChartBlock, TableBlock, TableCellValue } from "../../../types/chat";
import { CollapsibleSection } from "../CollapsibleSection/CollapsibleSection";
import { MultiSeriesLineChart } from "../charts/MultiSeriesLineChart/MultiSeriesLineChart";

interface ProductPerformanceChartProps {
  productChart: ChartBlock;
  variantChart: ChartBlock;
  datasets: TableBlock[];
}

interface VariantOption {
  id: string;
  title: string;
  parentId: string;
  parentName: string;
}

interface ProductGroup {
  id: string;
  name: string;
  variants: VariantOption[];
}

type PerformanceView = "products" | "variants";

const PRODUCT_METRICS = [
  "quantity_sold",
  "product_order_count",
  "product_unique_customers",
  "product_sales_value",
];

const VARIANT_METRICS = [
  ...PRODUCT_METRICS,
  "quantity_share_pct",
  "sales_value_share_pct",
  "order_penetration_pct",
  "buyer_penetration_pct",
];

const METRIC_LABELS: Record<string, string> = {
  quantity_sold: "Quantity sold",
  product_order_count: "Product orders",
  product_unique_customers: "Unique buyers",
  product_sales_value: "Sales value",
  quantity_share_pct: "Quantity share",
  sales_value_share_pct: "Sales value share",
  order_penetration_pct: "Order penetration",
  buyer_penetration_pct: "Buyer penetration",
};

const DISPLAY_LABEL_KEY = "__performance_series_label";

export function ProductPerformanceChart({
  productChart,
  variantChart,
  datasets,
}: ProductPerformanceChartProps) {
  const productDataset = datasets.find((dataset) => dataset.id === productChart.datasetId);
  const variantDataset = datasets.find((dataset) => dataset.id === variantChart.datasetId);
  const groups = useMemo(
    () => buildProductGroups(variantDataset, productDataset),
    [productDataset, variantDataset],
  );
  const [view, setView] = useState<PerformanceView>("products");
  const [activeParentId, setActiveParentId] = useState(() => groups[0]?.id ?? "");
  const [selectedVariantIds, setSelectedVariantIds] = useState<Set<string>>(
    () => new Set(groups[0]?.variants.slice(0, 3).map((variant) => variant.id) ?? []),
  );

  useEffect(() => {
    const availableVariantIds = new Set(
      groups.flatMap((group) => group.variants.map((variant) => variant.id)),
    );
    setActiveParentId((current) =>
      groups.some((group) => group.id === current) ? current : groups[0]?.id ?? ""
    );
    setSelectedVariantIds((current) =>
      new Set([...current].filter((id) => availableVariantIds.has(id)))
    );
  }, [groups]);

  const activeGroup = groups.find((group) => group.id === activeParentId) ?? groups[0];
  const selectedVariants = groups.flatMap((group) =>
    group.variants.filter((variant) => selectedVariantIds.has(variant.id))
  );
  const selectedParentCount = new Set(selectedVariants.map((variant) => variant.parentId)).size;

  const preparedProductDataset = useMemo(
    () => productDataset ? withMetricLabels(productDataset) : undefined,
    [productDataset],
  );
  const preparedProductChart = useMemo(
    () => productDataset ? prepareProductChart(productChart, productDataset) : productChart,
    [productChart, productDataset],
  );
  const preparedVariantDataset = useMemo(
    () => variantDataset
      ? prepareVariantDataset(variantDataset, selectedVariantIds, groups.length > 1)
      : undefined,
    [groups.length, selectedVariantIds, variantDataset],
  );
  const preparedVariantChart = useMemo(
    () => prepareMetricChart(variantChart, variantDataset, VARIANT_METRICS, DISPLAY_LABEL_KEY),
    [variantChart, variantDataset],
  );

  if (!productDataset || !variantDataset) {
    return (
      <CollapsibleSection title="Product performance">
        <div className="px-4 py-8 text-center text-sm text-ink-500">
          Performance chart data is unavailable.
        </div>
      </CollapsibleSection>
    );
  }

  return (
    <CollapsibleSection
      title="Product performance"
      subtitle={view === "products" ? "Parent product trends" : "Selected variant trends"}
    >
      <div className="border-b border-line px-4 py-3">
        <div className="inline-flex rounded-lg border border-line bg-panel-elevated p-1" role="group" aria-label="Performance level">
          {(["products", "variants"] as const).map((option) => (
            <button
              key={option}
              type="button"
              aria-pressed={view === option}
              onClick={() => setView(option)}
              className={`rounded-md px-3 py-1.5 text-xs font-semibold transition focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600 ${
                view === option
                  ? "bg-brand-100 text-brand-700 shadow-sm"
                  : "text-ink-600 hover:bg-brand-50 hover:text-ink-950"
              }`}
            >
              {option === "products" ? "Products" : "Variants"}
            </button>
          ))}
        </div>

        {view === "variants" && (
          <div className="mt-4 space-y-3">
            {groups.length === 0 ? (
              <p className="text-sm text-ink-500">No variants are available for selection.</p>
            ) : (
              <>
                <div className="grid gap-3 sm:grid-cols-[minmax(12rem,18rem)_minmax(0,1fr)] sm:items-start">
                  <label className="block text-xs font-medium text-ink-600">
                    Product
                    <select
                      value={activeGroup?.id ?? ""}
                      onChange={(event) => setActiveParentId(event.target.value)}
                      className="mt-1.5 w-full rounded-md border border-line bg-panel-elevated px-3 py-2 text-xs font-semibold text-ink-950 outline-none focus:border-brand-600 focus:ring-2 focus:ring-brand-100"
                    >
                      {groups.map((group) => (
                        <option key={group.id} value={group.id}>{group.name}</option>
                      ))}
                    </select>
                    {groups.length > 1 && (
                      <span className="mt-1.5 block text-[0.6875rem] font-normal leading-4 text-ink-500">
                        Selections from other products stay on the chart.
                      </span>
                    )}
                  </label>

                  <fieldset className="min-w-0">
                    <legend className="sr-only">Variants for {activeGroup?.name}</legend>
                    <div className="flex items-center justify-between gap-3">
                      <span className="text-xs font-medium text-ink-600">Variants</span>
                      {activeGroup && (
                        <button
                          type="button"
                          onClick={() => setSelectedVariantIds((current) => {
                            const next = new Set(current);
                            for (const variant of activeGroup.variants) next.delete(variant.id);
                            return next;
                          })}
                          className="rounded px-2 py-1 text-[0.6875rem] font-semibold text-ink-600 hover:bg-brand-50 hover:text-brand-600 focus-visible:outline-2 focus-visible:outline-brand-600"
                        >
                          Clear product
                        </button>
                      )}
                    </div>
                    <div className="chat-scrollbar mt-1.5 max-h-44 overflow-y-auto overscroll-contain rounded-lg border border-line bg-panel-elevated p-1.5">
                      {activeGroup?.variants.map((variant) => (
                        <label key={variant.id} className="flex items-start gap-2 rounded-md px-2 py-1.5 text-xs leading-4 text-ink-800 hover:bg-brand-50">
                          <input
                            type="checkbox"
                            checked={selectedVariantIds.has(variant.id)}
                            onChange={() => setSelectedVariantIds((current) => {
                              const next = new Set(current);
                              if (next.has(variant.id)) next.delete(variant.id); else next.add(variant.id);
                              return next;
                            })}
                            className="mt-0.5 accent-[var(--accent)]"
                          />
                          <span>{variant.title}</span>
                        </label>
                      ))}
                    </div>
                  </fieldset>
                </div>

                <div>
                  <div className="flex flex-wrap items-center justify-between gap-2">
                    <p className="text-[0.6875rem] font-semibold uppercase tracking-[0.06em] text-ink-500">
                      Selected variants
                    </p>
                    <span className="text-[0.6875rem] text-ink-500">
                      {selectedVariants.length} across {selectedParentCount} {selectedParentCount === 1 ? "product" : "products"}
                    </span>
                  </div>
                  {selectedVariants.length > 0 ? (
                    <div className="mt-2 flex flex-wrap gap-1.5">
                      {selectedVariants.map((variant) => (
                        <button
                          key={variant.id}
                          type="button"
                          title={`Remove ${variant.parentName} · ${variant.title}`}
                          onClick={() => setSelectedVariantIds((current) => {
                            const next = new Set(current);
                            next.delete(variant.id);
                            return next;
                          })}
                          className="max-w-full rounded-full border border-line bg-panel-elevated px-2.5 py-1 text-left text-[0.6875rem] text-ink-600 hover:border-brand-600 hover:text-brand-600 focus-visible:outline-2 focus-visible:outline-brand-600"
                        >
                          <span className="break-words">
                            {groups.length > 1 ? `${variant.parentName} · ` : ""}{variant.title} ×
                          </span>
                        </button>
                      ))}
                    </div>
                  ) : (
                    <p className="mt-2 text-xs text-ink-500">Choose a small set of variants to display.</p>
                  )}
                </div>
              </>
            )}
          </div>
        )}
      </div>

      <div hidden={view !== "products"}>
        {preparedProductDataset && (
          <MultiSeriesLineChart chart={preparedProductChart} dataset={preparedProductDataset} />
        )}
      </div>
      <div hidden={view !== "variants"}>
        {preparedVariantDataset && (
          selectedVariantIds.size > 0
            ? <MultiSeriesLineChart chart={preparedVariantChart} dataset={preparedVariantDataset} />
            : <div className="px-4 py-10 text-center text-sm text-ink-500">Select one or more variants to draw their trends.</div>
        )}
      </div>
    </CollapsibleSection>
  );
}

function buildProductGroups(
  variantDataset: TableBlock | undefined,
  productDataset: TableBlock | undefined,
): ProductGroup[] {
  if (!variantDataset) return [];
  const fallbackParentId = firstString(productDataset?.rows, "product_id");
  const fallbackParentName = firstString(productDataset?.rows, "product_name");
  const groups = new Map<string, ProductGroup>();

  for (const row of variantDataset.rows) {
    const variantId = cellString(row.variant_id);
    const variantTitle = cellString(row.variant_title) || variantId;
    const parentId = cellString(row.parent_product_id) || fallbackParentId;
    const parentName = cellString(row.parent_product_name) || fallbackParentName || parentId;
    if (!variantId || !parentId) continue;

    const group = groups.get(parentId) ?? { id: parentId, name: parentName, variants: [] };
    if (!group.variants.some((variant) => variant.id === variantId)) {
      group.variants.push({ id: variantId, title: variantTitle, parentId, parentName });
    }
    groups.set(parentId, group);
  }

  return [...groups.values()];
}

function prepareProductChart(chart: ChartBlock, dataset: TableBlock): ChartBlock {
  return prepareMetricChart(chart, dataset, PRODUCT_METRICS, "product_name", "product_id");
}

function prepareMetricChart(
  chart: ChartBlock,
  dataset: TableBlock | undefined,
  metricKeys: string[],
  seriesLabelKey: string,
  seriesKey = chart.seriesKey,
): ChartBlock {
  const availableYKeys = metricKeys.filter((key) =>
    dataset?.columns.some((column) => column.key === key)
  );
  const yKey = availableYKeys.includes(chart.yKey) ? chart.yKey : availableYKeys[0] ?? chart.yKey;
  return {
    ...chart,
    yKey,
    seriesKey,
    seriesLabelKey,
    availableYKeys,
  };
}

function prepareVariantDataset(
  dataset: TableBlock,
  selectedVariantIds: Set<string>,
  qualifyLabels: boolean,
): TableBlock {
  const rows = dataset.rows
    .filter((row) => selectedVariantIds.has(cellString(row.variant_id)))
    .map((row) => {
      const title = cellString(row.variant_title) || cellString(row.variant_id);
      const parentName = cellString(row.parent_product_name);
      return {
        ...row,
        [DISPLAY_LABEL_KEY]: qualifyLabels && parentName ? `${parentName} · ${title}` : title,
      };
    });
  return withMetricLabels({
    ...dataset,
    columns: [
      ...dataset.columns,
      { key: DISPLAY_LABEL_KEY, label: "Series", type: "string" },
    ],
    rows,
    rowCount: rows.length,
  });
}

function withMetricLabels(dataset: TableBlock): TableBlock {
  return {
    ...dataset,
    columns: dataset.columns.map((column) => ({
      ...column,
      label: METRIC_LABELS[column.key] ?? column.label,
    })),
  };
}

function firstString(
  rows: Array<Record<string, TableCellValue>> | undefined,
  key: string,
): string {
  return rows?.map((row) => cellString(row[key])).find(Boolean) ?? "";
}

function cellString(value: TableCellValue | undefined): string {
  if (value === null || value === undefined) return "";
  return String(value).trim();
}
