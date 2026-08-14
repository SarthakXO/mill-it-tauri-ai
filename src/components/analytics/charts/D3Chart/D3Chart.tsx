import { useEffect, useMemo, useRef, useState } from "react";
import {
  axisBottom,
  axisLeft,
  format,
  line,
  max,
  scaleBand,
  scaleLinear,
  scalePoint,
  select,
} from "d3";
import type { ChartBlock, TableBlock } from "../../../../types/chat";
import type { Selection } from "d3";
import { useTheme } from "../../../theme/ThemeProvider";

interface D3ChartProps {
  chart: ChartBlock;
  dataset: TableBlock;
}

interface ChartDatum {
  label: string;
  value: number;
}

export function D3Chart({ chart, dataset }: D3ChartProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const [width, setWidth] = useState(640);
  const { theme } = useTheme();
  const data = useMemo(
    () => dataset.rows
      .flatMap((row): ChartDatum[] => {
        const rawValue = row[chart.yKey];
        if (typeof rawValue !== "number" || !Number.isFinite(rawValue)) return [];
        const rawLabel = row[chart.xKey];
        return [{ label: rawLabel === null ? "Unspecified" : String(rawLabel), value: rawValue }];
      })
      .slice(0, 60),
    [chart.xKey, chart.yKey, dataset.rows],
  );

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const observer = new ResizeObserver(([entry]) => {
      setWidth(Math.max(320, Math.floor(entry.contentRect.width)));
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const node = svgRef.current;
    if (!node || data.length === 0) return;
    const svg = select(node);
    svg.selectAll("*").remove();
    const styles = getComputedStyle(document.documentElement);
    const colors: ChartColors = {
      accent: styles.getPropertyValue("--accent").trim(),
      accentStrong: styles.getPropertyValue("--accent-strong").trim(),
      surface: styles.getPropertyValue("--surface").trim(),
      muted: styles.getPropertyValue("--muted").trim(),
      grid: styles.getPropertyValue("--chart-grid").trim(),
      axis: styles.getPropertyValue("--chart-axis").trim(),
    };
    if (chart.type === "horizontal_bar") {
      drawHorizontalBars(svg, data, width, colors);
    } else if (chart.type === "line") {
      drawLine(svg, data, width, colors);
    } else {
      drawBars(svg, data, width, colors);
    }
  }, [chart.type, data, theme, width]);

  return (
    <div className="p-4">
      {data.length === 0 ? (
        <div className="py-10 text-center text-sm text-ink-500">No chartable values.</div>
      ) : (
        <div ref={containerRef} className="min-h-64 w-full overflow-hidden">
          <svg
            ref={svgRef}
            className="block w-full"
            role="img"
            aria-label={chart.title}
          />
        </div>
      )}
    </div>
  );
}

type SvgSelection = Selection<SVGSVGElement, unknown, null, undefined>;

interface ChartColors {
  accent: string;
  accentStrong: string;
  surface: string;
  muted: string;
  grid: string;
  axis: string;
}

function drawHorizontalBars(svg: SvgSelection, data: ChartDatum[], width: number, colors: ChartColors) {
  const height = Math.max(260, data.length * 38 + 62);
  const longestLabel = max(data, (datum) => datum.label.length) ?? 10;
  const margin = { top: 10, right: 34, bottom: 42, left: Math.min(220, Math.max(112, longestLabel * 6.5)) };
  const x = scaleLinear()
    .domain([0, max(data, (datum) => datum.value) ?? 0])
    .nice()
    .range([margin.left, width - margin.right]);
  const y = scaleBand()
    .domain(data.map((datum) => datum.label))
    .range([margin.top, height - margin.bottom])
    .padding(0.24);

  configureSvg(svg, width, height);
  svg
    .append("g")
    .attr("transform", `translate(0,${height - margin.bottom})`)
    .call(axisBottom(x).ticks(Math.max(3, width / 120)).tickFormat((value) => format("~s")(Number(value))))
    .call((selection) => styleAxis(selection, colors));
  svg
    .append("g")
    .attr("transform", `translate(${margin.left},0)`)
    .call(axisLeft(y).tickFormat(shortLabel))
    .call((selection) => styleAxis(selection, colors));
  const bars = svg
    .append("g")
    .selectAll("rect")
    .data(data)
    .join("rect")
    .attr("x", margin.left)
    .attr("y", (datum) => y(datum.label) ?? 0)
    .attr("width", (datum) => Math.max(0, x(datum.value) - margin.left))
    .attr("height", y.bandwidth())
    .attr("rx", 3)
    .attr("fill", colors.accent);
  bars.append("title").text((datum) => `${datum.label}: ${datum.value.toLocaleString("en-IN")}`);
}

function drawBars(svg: SvgSelection, data: ChartDatum[], width: number, colors: ChartColors) {
  const height = 290;
  const margin = { top: 10, right: 24, bottom: 72, left: 58 };
  const x = scaleBand()
    .domain(data.map((datum) => datum.label))
    .range([margin.left, width - margin.right])
    .padding(0.3);
  const y = scaleLinear()
    .domain([0, max(data, (datum) => datum.value) ?? 0])
    .nice()
    .range([height - margin.bottom, margin.top]);

  configureSvg(svg, width, height);
  svg
    .append("g")
    .attr("transform", `translate(0,${height - margin.bottom})`)
    .call(axisBottom(x).tickFormat(shortLabel))
    .call((selection) => styleAxis(selection, colors))
    .selectAll("text")
    .attr("transform", "rotate(-25)")
    .style("text-anchor", "end");
  svg
    .append("g")
    .attr("transform", `translate(${margin.left},0)`)
    .call(axisLeft(y).ticks(5).tickFormat((value) => format("~s")(Number(value))))
    .call((selection) => styleAxis(selection, colors));
  const bars = svg
    .append("g")
    .selectAll("rect")
    .data(data)
    .join("rect")
    .attr("x", (datum) => x(datum.label) ?? 0)
    .attr("y", (datum) => y(datum.value))
    .attr("width", x.bandwidth())
    .attr("height", (datum) => height - margin.bottom - y(datum.value))
    .attr("rx", 3)
    .attr("fill", colors.accent);
  bars.append("title").text((datum) => `${datum.label}: ${datum.value.toLocaleString("en-IN")}`);
}

function drawLine(svg: SvgSelection, data: ChartDatum[], width: number, colors: ChartColors) {
  const height = 290;
  const margin = { top: 16, right: 28, bottom: 68, left: 58 };
  const x = scalePoint()
    .domain(data.map((datum) => datum.label))
    .range([margin.left, width - margin.right])
    .padding(0.35);
  const y = scaleLinear()
    .domain([0, max(data, (datum) => datum.value) ?? 0])
    .nice()
    .range([height - margin.bottom, margin.top]);
  const path = line<ChartDatum>()
    .x((datum) => x(datum.label) ?? margin.left)
    .y((datum) => y(datum.value));

  configureSvg(svg, width, height);
  svg
    .append("g")
    .attr("transform", `translate(0,${height - margin.bottom})`)
    .call(axisBottom(x).tickValues(sampleLabels(data.map((datum) => datum.label), width)).tickFormat(shortLabel))
    .call((selection) => styleAxis(selection, colors))
    .selectAll("text")
    .attr("transform", "rotate(-25)")
    .style("text-anchor", "end");
  svg
    .append("g")
    .attr("transform", `translate(${margin.left},0)`)
    .call(axisLeft(y).ticks(5).tickFormat((value) => format("~s")(Number(value))))
    .call((selection) => styleAxis(selection, colors));
  svg
    .append("path")
    .datum(data)
    .attr("fill", "none")
    .attr("stroke", colors.accent)
    .attr("stroke-width", 2.25)
    .attr("stroke-linejoin", "round")
    .attr("stroke-linecap", "round")
    .attr("d", path);
  const points = svg
    .append("g")
    .selectAll("circle")
    .data(data)
    .join("circle")
    .attr("cx", (datum) => x(datum.label) ?? margin.left)
    .attr("cy", (datum) => y(datum.value))
    .attr("r", data.length === 1 ? 5 : 3.5)
    .attr("fill", colors.surface)
    .attr("stroke", colors.accentStrong)
    .attr("stroke-width", 2);
  points.append("title").text((datum) => `${datum.label}: ${datum.value.toLocaleString("en-IN")}`);
}

function configureSvg(svg: SvgSelection, width: number, height: number) {
  svg.attr("viewBox", `0 0 ${width} ${height}`).attr("height", height);
}

function styleAxis(selection: Selection<SVGGElement, unknown, null, undefined>, colors: ChartColors) {
  selection.selectAll("text").attr("fill", colors.muted).style("font-size", "11px");
  selection.selectAll("line").attr("stroke", colors.grid);
  selection.select(".domain").attr("stroke", colors.axis);
}

function shortLabel(value: string): string {
  return value.length > 24 ? `${value.slice(0, 22)}…` : value;
}

function sampleLabels(labels: string[], width: number): string[] {
  const maximum = Math.max(3, Math.floor(width / 85));
  if (labels.length <= maximum) return labels;
  const step = Math.ceil(labels.length / maximum);
  return labels.filter((_, index) => index % step === 0 || index === labels.length - 1);
}
