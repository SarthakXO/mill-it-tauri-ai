import type { ChartBlock, TableBlock } from "../../../types/chat";
import { CollapsibleSection } from "../CollapsibleSection/CollapsibleSection";
import { D3Chart } from "../charts/D3Chart/D3Chart";
import { MultiSeriesLineChart } from "../charts/MultiSeriesLineChart/MultiSeriesLineChart";

interface ChartRendererProps {
  chart: ChartBlock;
  datasets: TableBlock[];
}

export function ChartRenderer({ chart, datasets }: ChartRendererProps) {
  const dataset = datasets.find((candidate) => candidate.id === chart.datasetId);
  if (!dataset) {
    return (
      <CollapsibleSection title={chart.title} subtitle={`${chart.xLabel} · ${chart.yLabel}`}>
        <div className="px-4 py-8 text-center text-sm text-ink-500">Chart data is unavailable.</div>
      </CollapsibleSection>
    );
  }

  const isMultiSeries = chart.type === "line" && Boolean(chart.seriesKey);

  return (
    <CollapsibleSection title={chart.title} subtitle={`${chart.xLabel} · ${chart.yLabel}`}>
      {isMultiSeries ? <MultiSeriesLineChart chart={chart} dataset={dataset} /> : <D3Chart chart={chart} dataset={dataset} />}
    </CollapsibleSection>
  );
}
