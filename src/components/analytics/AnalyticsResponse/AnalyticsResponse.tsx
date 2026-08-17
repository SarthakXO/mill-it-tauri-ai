import type { AgentResponse } from "../../../types/chat";
import { ChartRenderer } from "../ChartRenderer/ChartRenderer";
import { DataTable } from "../DataTable/DataTable";

interface AnalyticsResponseProps {
  response: AgentResponse;
}

export function AnalyticsResponse({ response }: AnalyticsResponseProps) {
  const tables = response.tables ?? [];
  const charts = response.charts ?? [];
  const warnings = response.warnings ?? [];

  return (
    <div className="space-y-5">
      <div className="whitespace-pre-wrap break-words text-[0.9375rem] leading-6 text-ink-800">
        {response.answer}
      </div>

      {tables.map((table) => (
        <DataTable key={table.id} table={table} />
      ))}

      {charts.map((chart) => (
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

      {response.dataAsOf && (
        <p className="text-[0.6875rem] text-ink-500">Data as of {response.dataAsOf}</p>
      )}
    </div>
  );
}
