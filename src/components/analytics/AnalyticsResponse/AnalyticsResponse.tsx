import type { AgentResponse } from "../../../types/chat";
import { ChartRenderer } from "../ChartRenderer/ChartRenderer";
import { DataTable } from "../DataTable/DataTable";
import { ProductPerformanceChart } from "../ProductPerformanceChart/ProductPerformanceChart";

interface AnalyticsResponseProps {
  response: AgentResponse;
}

export function AnalyticsResponse({ response }: AnalyticsResponseProps) {
  const tables = response.tables ?? [];
  const charts = response.charts ?? [];
  const warnings = response.warnings ?? [];
  const productChart = charts.find((chart) =>
    chart.datasetId === "product-performance" || chart.datasetId === "product-comparison"
  );
  const variantDatasetId = productChart?.datasetId === "product-comparison"
    ? "variant-comparison"
    : "variant-performance";
  const variantChart = productChart
    ? charts.find((chart) => chart.datasetId === variantDatasetId)
    : undefined;
  const groupedPerformanceChartIds = new Set(
    productChart && variantChart ? [productChart.id, variantChart.id] : [],
  );

  return (
    <div className="space-y-5">
      <div className="whitespace-pre-wrap break-words text-[0.9375rem] leading-6 text-ink-800">
        {response.answer}
      </div>

      {productChart && variantChart && (
        <ProductPerformanceChart
          productChart={productChart}
          variantChart={variantChart}
          datasets={tables}
        />
      )}

      {charts.filter((chart) => !groupedPerformanceChartIds.has(chart.id)).map((chart) => (
        <ChartRenderer key={chart.id} chart={chart} datasets={tables} />
      ))}

      {warnings.map((warning, index) => (
        <div
          key={`${warning.message}-${index}`}
          className="rounded-lg border border-warning-border bg-warning px-3.5 py-3 text-xs leading-5 text-warning-text"
          role="note"
        >
          {warning.message}
        </div>
      ))}

      {tables.map((table) => (
        <DataTable key={table.id} table={table} />
      ))}

      {response.dataAsOf && (
        <p className="text-[0.6875rem] text-ink-500">Data as of {response.dataAsOf}</p>
      )}
    </div>
  );
}
