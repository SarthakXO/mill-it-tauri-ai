import type { ChartBlock, TableBlock, TableColumn } from "../../../../types/chat";
import { formatCell, isNumericColumn } from "../../DataTable/tableUtils";

export interface SeriesPoint {
  x: string;
  value: number | null;
}

export interface ChartSeries {
  id: string;
  label: string;
  points: SeriesPoint[];
}

export interface MultiSeriesData {
  xValues: string[];
  series: ChartSeries[];
}

export function getMetricColumns(chart: ChartBlock, dataset: TableBlock): TableColumn[] {
  const requestedKeys = chart.availableYKeys?.length ? chart.availableYKeys : [chart.yKey];
  const keys = [...new Set([chart.yKey, ...requestedKeys])];
  return keys.flatMap((key) => {
    const column = dataset.columns.find((candidate) => candidate.key === key);
    return column && isNumericColumn(column) ? [column] : [];
  });
}

export function buildMultiSeriesData(chart: ChartBlock, dataset: TableBlock, metricKey: string): MultiSeriesData {
  if (!chart.seriesKey) return { xValues: [], series: [] };
  const xColumn = dataset.columns.find((column) => column.key === chart.xKey);
  const xValues = [...new Set(dataset.rows.flatMap((row) => {
    const value = row[chart.xKey];
    return value === null || value === "" ? [] : [String(value)];
  }))].sort((left, right) => compareX(left, right, xColumn));

  const labels = new Map<string, string>();
  const values = new Map<string, Map<string, number>>();
  for (const row of dataset.rows) {
    const rawSeriesId = row[chart.seriesKey];
    const rawX = row[chart.xKey];
    if (rawSeriesId === null || rawSeriesId === "" || rawX === null || rawX === "") continue;
    const id = String(rawSeriesId);
    const rawLabel = chart.seriesLabelKey ? row[chart.seriesLabelKey] : null;
    const label = rawLabel === null || String(rawLabel).trim() === "" ? id : String(rawLabel);
    if (!labels.has(id) || labels.get(id) === id) labels.set(id, label);
    const rawValue = row[metricKey];
    if (typeof rawValue !== "number" || !Number.isFinite(rawValue)) continue;
    const seriesValues = values.get(id) ?? new Map<string, number>();
    seriesValues.set(String(rawX), rawValue);
    values.set(id, seriesValues);
  }

  return {
    xValues,
    series: [...labels].map(([id, label]) => ({
      id,
      label,
      points: xValues.map((x) => ({
        x,
        value: values.get(id)?.get(x) ?? (chart.zeroFillMissingPoints ? 0 : null),
      })),
    })),
  };
}

export function formatMetric(value: number, column: TableColumn): string {
  return formatCell(value, column);
}

export function formatXAxis(value: string, column: TableColumn | undefined, compact = false): string {
  if (column?.type === "date" || column?.type === "datetime") {
    const date = new Date(value);
    if (!Number.isNaN(date.getTime())) {
      return new Intl.DateTimeFormat("en-IN", compact
        ? { month: "short", year: "2-digit", timeZone: "Asia/Kolkata" }
        : { month: "long", year: "numeric", timeZone: "Asia/Kolkata" }).format(date);
    }
  }
  return value;
}

function compareX(left: string, right: string, column: TableColumn | undefined): number {
  if (column?.type === "date" || column?.type === "datetime") {
    const difference = new Date(left).getTime() - new Date(right).getTime();
    if (Number.isFinite(difference)) return difference;
  }
  return left.localeCompare(right, undefined, { numeric: true, sensitivity: "base" });
}

export function seriesColor(id: string, darkTheme: boolean): string {
  let hash = 2166136261;
  for (let index = 0; index < id.length; index += 1) {
    hash ^= id.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  const hue = Math.abs(hash) % 360;
  return `hsl(${hue} 68% ${darkTheme ? 67 : 41}%)`;
}
