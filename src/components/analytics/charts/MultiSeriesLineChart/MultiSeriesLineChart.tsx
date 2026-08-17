import { useEffect, useMemo, useRef, useState, type PointerEvent } from "react";
import { format, line, max, min, scaleLinear, scalePoint } from "d3";
import type { ChartBlock, TableBlock } from "../../../../types/chat";
import { useTheme } from "../../../theme/ThemeProvider";
import { buildMultiSeriesData, formatMetric, formatXAxis, getMetricColumns, seriesColor, type ChartSeries, type SeriesPoint } from "./chartData";

interface MultiSeriesLineChartProps {
  chart: ChartBlock;
  dataset: TableBlock;
}

interface TooltipState {
  seriesId: string;
  point: SeriesPoint;
  left: number;
  top: number;
}

export function MultiSeriesLineChart({ chart, dataset }: MultiSeriesLineChartProps) {
  const metricColumns = useMemo(() => getMetricColumns(chart, dataset), [chart, dataset]);
  const [metricKey, setMetricKey] = useState(chart.yKey);
  const metricColumn = metricColumns.find((column) => column.key === metricKey) ?? metricColumns[0];
  const data = useMemo(() => buildMultiSeriesData(chart, dataset, metricColumn?.key ?? chart.yKey), [chart, dataset, metricColumn?.key]);
  const [visibleSeries, setVisibleSeries] = useState(() => new Set(data.series.map((series) => series.id)));
  const [hoveredSeries, setHoveredSeries] = useState<string | null>(null);
  const [tooltip, setTooltip] = useState<TooltipState | null>(null);
  const { theme } = useTheme();

  useEffect(() => {
    setVisibleSeries((current) => {
      const validIds = new Set(data.series.map((series) => series.id));
      return new Set([...current].filter((id) => validIds.has(id)));
    });
  }, [data.series]);

  if (!metricColumn || data.series.length === 0 || data.xValues.length === 0) {
    return <div className="px-4 py-10 text-center text-sm text-ink-500">No multi-series values are available.</div>;
  }

  return (
    <div className="p-4">
      <div className="mb-4 flex flex-wrap items-end justify-between gap-3">
        <div>
          <p className="text-[0.6875rem] font-medium uppercase tracking-[0.06em] text-ink-500">{chart.xLabel}</p>
          <p className="mt-0.5 text-xs text-ink-600">{metricColumn.label}</p>
        </div>
        {metricColumns.length > 1 && (
          <label className="flex items-center gap-2 text-xs font-medium text-ink-600">
            Metric
            <select value={metricColumn.key} onChange={(event) => setMetricKey(event.target.value)} className="rounded-md border border-line bg-panel-elevated px-3 py-2 text-xs font-semibold text-ink-950 outline-none focus:border-brand-600 focus:ring-2 focus:ring-brand-100">
              {metricColumns.map((column) => <option key={column.key} value={column.key}>{column.label}</option>)}
            </select>
          </label>
        )}
      </div>

      <div className="grid gap-4 min-[760px]:grid-cols-[minmax(0,1fr)_minmax(11rem,14rem)]">
        <LinePlot
          chart={chart}
          dataset={dataset}
          series={data.series}
          xValues={data.xValues}
          metricKey={metricColumn.key}
          visibleSeries={visibleSeries}
          hoveredSeries={hoveredSeries}
          tooltip={tooltip}
          darkTheme={theme === "dark"}
          onHoverSeries={setHoveredSeries}
          onTooltip={setTooltip}
        />

        <aside className="min-w-0 border-t border-line pt-3 min-[760px]:border-l min-[760px]:border-t-0 min-[760px]:pl-4 min-[760px]:pt-0" aria-label="Chart legend">
          <div className="mb-2 flex items-center justify-between gap-2">
            <span className="text-[0.6875rem] font-semibold uppercase tracking-[0.06em] text-ink-500">Series</span>
            <span className="text-[0.6875rem] text-ink-500">{visibleSeries.size}/{data.series.length}</span>
          </div>
          <div className="mb-2 flex gap-1.5">
            <button type="button" onClick={() => setVisibleSeries(new Set(data.series.map((series) => series.id)))} className="rounded px-2 py-1 text-[0.6875rem] font-semibold text-brand-600 hover:bg-brand-50 focus-visible:outline-2 focus-visible:outline-brand-600">Show all</button>
            <button type="button" onClick={() => { setVisibleSeries(new Set()); setTooltip(null); }} className="rounded px-2 py-1 text-[0.6875rem] font-semibold text-ink-600 hover:bg-brand-50 focus-visible:outline-2 focus-visible:outline-brand-600">Hide all</button>
          </div>
          <div className="chat-scrollbar max-h-[19rem] space-y-0.5 overflow-y-auto pr-1">
            {data.series.map((series) => {
              const visible = visibleSeries.has(series.id);
              const active = hoveredSeries === series.id;
              return (
                <button
                  key={series.id}
                  type="button"
                  aria-pressed={visible}
                  title={series.label}
                  onClick={() => setVisibleSeries((current) => {
                    const next = new Set(current);
                    if (next.has(series.id)) next.delete(series.id); else next.add(series.id);
                    return next;
                  })}
                  onPointerEnter={() => visible && setHoveredSeries(series.id)}
                  onPointerLeave={() => { setHoveredSeries(null); setTooltip(null); }}
                  onFocus={() => visible && setHoveredSeries(series.id)}
                  onBlur={() => setHoveredSeries(null)}
                  className={`flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left text-xs leading-4 transition focus-visible:outline-2 focus-visible:outline-brand-600 ${active ? "bg-brand-100 text-ink-950" : "hover:bg-brand-50"} ${visible ? "text-ink-800" : "text-ink-500 opacity-55"}`}
                >
                  <span className="mt-1 size-2 shrink-0 rounded-full" style={{ backgroundColor: seriesColor(series.id, theme === "dark") }} aria-hidden="true" />
                  <span className={visible ? "" : "line-through"}>{series.label}</span>
                </button>
              );
            })}
          </div>
        </aside>
      </div>
    </div>
  );
}

interface LinePlotProps {
  chart: ChartBlock;
  dataset: TableBlock;
  series: ChartSeries[];
  xValues: string[];
  metricKey: string;
  visibleSeries: Set<string>;
  hoveredSeries: string | null;
  tooltip: TooltipState | null;
  darkTheme: boolean;
  onHoverSeries: (id: string | null) => void;
  onTooltip: (tooltip: TooltipState | null) => void;
}

function LinePlot({ chart, dataset, series, xValues, metricKey, visibleSeries, hoveredSeries, tooltip, darkTheme, onHoverSeries, onTooltip }: LinePlotProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(620);
  const height = 320;
  const margin = { top: 14, right: 16, bottom: 56, left: 64 };
  const xColumn = dataset.columns.find((column) => column.key === chart.xKey);
  const metricColumn = dataset.columns.find((column) => column.key === metricKey)!;
  const visible = series.filter((item) => visibleSeries.has(item.id));
  const visibleValues = visible.flatMap((item) => item.points.flatMap((point) => point.value === null ? [] : [point.value]));
  const minimum = min(visibleValues) ?? 0;
  const maximum = max(visibleValues) ?? 0;
  const y = scaleLinear().domain([Math.min(0, minimum), Math.max(0, maximum)]).nice().range([height - margin.bottom, margin.top]);
  const x = scalePoint<string>().domain(xValues).range([margin.left, width - margin.right]).padding(0.35);
  const path = line<SeriesPoint>().defined((point) => point.value !== null).x((point) => x(point.x) ?? margin.left).y((point) => y(point.value ?? 0));
  const yTicks = y.ticks(5);
  const xTicks = sampleLabels(xValues, width);

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(320, Math.floor(entry.contentRect.width))));
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const showTooltip = (event: PointerEvent<SVGPathElement>, item: ChartSeries) => {
    const svg = event.currentTarget.ownerSVGElement;
    if (!svg) return;
    const rect = svg.getBoundingClientRect();
    const pointerX = (event.clientX - rect.left) * (width / rect.width);
    const point = item.points.reduce((nearest, candidate) => Math.abs((x(candidate.x) ?? 0) - pointerX) < Math.abs((x(nearest.x) ?? 0) - pointerX) ? candidate : nearest);
    if (point.value === null) return onTooltip(null);
    onTooltip({ seriesId: item.id, point, left: x(point.x) ?? pointerX, top: y(point.value) });
  };

  return (
    <div ref={containerRef} className="relative min-h-80 min-w-0 overflow-hidden">
      <svg viewBox={`0 0 ${width} ${height}`} height={height} className="block w-full" role="img" aria-label={`${chart.title}, ${metricColumn.label}`}>
        <g aria-hidden="true">
          {yTicks.map((tick) => (
            <g key={tick}>
              <line x1={margin.left} x2={width - margin.right} y1={y(tick)} y2={y(tick)} stroke="var(--chart-grid)" />
              <text x={margin.left - 10} y={y(tick)} dy="0.32em" textAnchor="end" fill="var(--muted)" fontSize="11">{format("~s")(tick)}</text>
            </g>
          ))}
          <line x1={margin.left} x2={width - margin.right} y1={height - margin.bottom} y2={height - margin.bottom} stroke="var(--chart-axis)" />
          {xTicks.map((tick) => (
            <text key={tick} x={x(tick)} y={height - margin.bottom + 22} textAnchor="middle" fill="var(--muted)" fontSize="11">{shortLabel(formatXAxis(tick, xColumn, true))}</text>
          ))}
        </g>
        {visible.map((item) => {
          const active = hoveredSeries === item.id;
          const dimmed = hoveredSeries !== null && !active;
          const color = seriesColor(item.id, darkTheme);
          const d = path(item.points) ?? undefined;
          return (
            <g key={item.id} opacity={dimmed ? 0.18 : 1}>
              <path d={d} fill="none" stroke={color} strokeWidth={active ? 3.5 : 2.15} strokeLinejoin="round" strokeLinecap="round" className="transition-[opacity,stroke-width] duration-150" />
              <path
                d={d}
                fill="none"
                stroke="transparent"
                strokeWidth="14"
                pointerEvents="stroke"
                tabIndex={0}
                aria-label={item.label}
                onPointerEnter={(event) => { onHoverSeries(item.id); showTooltip(event, item); }}
                onPointerMove={(event) => showTooltip(event, item)}
                onPointerLeave={() => { onHoverSeries(null); onTooltip(null); }}
                onFocus={() => onHoverSeries(item.id)}
                onBlur={() => onHoverSeries(null)}
              />
              {item.points.filter((point) => point.value !== null).map((point) => (
                <circle key={point.x} cx={x(point.x)} cy={y(point.value ?? 0)} r={tooltip?.seriesId === item.id && tooltip.point.x === point.x ? 4.5 : 2.5} fill="var(--surface)" stroke={color} strokeWidth="2" pointerEvents="none" />
              ))}
            </g>
          );
        })}
      </svg>
      {visible.length === 0 && <div className="pointer-events-none absolute inset-0 grid place-items-center text-sm text-ink-500">Select a series from the legend.</div>}
      {tooltip && (() => {
        const item = series.find((candidate) => candidate.id === tooltip.seriesId);
        if (!item || tooltip.point.value === null) return null;
        return (
          <div className="pointer-events-none absolute z-20 w-max max-w-64 rounded-lg border border-line bg-[var(--chart-tooltip-background)] px-3 py-2 text-xs text-[var(--chart-tooltip-text)] shadow-xl" style={{ left: Math.min(Math.max(8, tooltip.left + 12), Math.max(8, width - 230)), top: Math.max(4, tooltip.top - 78) }}>
            <p className="font-semibold leading-4">{item.label}</p>
            <p className="mt-1 opacity-75">{formatXAxis(tooltip.point.x, xColumn)}</p>
            <p className="mt-1"><span className="opacity-75">{metricColumn.label}:</span> <span className="font-semibold tabular-nums">{formatMetric(tooltip.point.value, metricColumn)}</span></p>
          </div>
        );
      })()}
    </div>
  );
}

function shortLabel(value: string): string {
  return value.length > 14 ? `${value.slice(0, 12)}…` : value;
}

function sampleLabels(labels: string[], width: number): string[] {
  const maximum = Math.max(3, Math.floor(width / 85));
  if (labels.length <= maximum) return labels;
  const step = Math.ceil(labels.length / maximum);
  return labels.filter((_, index) => index % step === 0 || index === labels.length - 1);
}
