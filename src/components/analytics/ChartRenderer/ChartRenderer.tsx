import type { ChartBlock, TableBlock } from "../../../types/chat";
import { D3Chart } from "../charts/D3Chart/D3Chart";

interface ChartRendererProps {
  chart: ChartBlock;
  datasets: TableBlock[];
}

export function ChartRenderer({ chart, datasets }: ChartRendererProps) {
  const dataset = datasets.find((candidate) => candidate.id === chart.datasetId);
  if (!dataset) {
    return (
      <div className="rounded-xl border border-line bg-white px-4 py-8 text-center text-sm text-ink-500">
        Chart data is unavailable.
      </div>
    );
  }

  return <D3Chart chart={chart} dataset={dataset} />;
}
