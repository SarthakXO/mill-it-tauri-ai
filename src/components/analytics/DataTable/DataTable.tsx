import { useEffect, useRef, useState, type MouseEvent } from "react";
import type { TableBlock } from "../../../types/chat";
import { CollapsibleSection } from "../CollapsibleSection/CollapsibleSection";
import { CsvExportDialog } from "./CsvExportDialog";
import { copyText, exportTableCsv, formatCell, isNumericColumn, isSortableColumn, rowToTsv, tableToTsv } from "./tableUtils";
import { useTableSort } from "./useTableSort";

interface DataTableProps {
  table: TableBlock;
}

type CopyTarget = "table" | "row" | null;
type CopyResult = "idle" | "copied" | "failed";

export { tableToTsv } from "./tableUtils";

export function DataTable({ table }: DataTableProps) {
  const [selectedRowIndex, setSelectedRowIndex] = useState<number | null>(null);
  const [copyTarget, setCopyTarget] = useState<CopyTarget>(null);
  const [copyResult, setCopyResult] = useState<CopyResult>("idle");
  const [showExportDialog, setShowExportDialog] = useState(false);
  const resetTimer = useRef<number | undefined>(undefined);
  const { rows, sort, toggleSort } = useTableSort(table);
  const selectedRow = selectedRowIndex === null ? null : table.rows[selectedRowIndex] ?? null;

  useEffect(() => () => {
    if (resetTimer.current !== undefined) window.clearTimeout(resetTimer.current);
  }, []);

  const copy = async (target: Exclude<CopyTarget, null>, value: string) => {
    setCopyTarget(target);
    try {
      await copyText(value);
      setCopyResult("copied");
    } catch {
      setCopyResult("failed");
    }
    if (resetTimer.current !== undefined) window.clearTimeout(resetTimer.current);
    resetTimer.current = window.setTimeout(() => {
      setCopyTarget(null);
      setCopyResult("idle");
    }, 1800);
  };

  const copyLabel = (target: Exclude<CopyTarget, null>, idleLabel: string) => {
    if (copyTarget !== target || copyResult === "idle") return idleLabel;
    return copyResult === "copied" ? "Copied" : "Try again";
  };

  const handleRowClick = (event: MouseEvent<HTMLTableRowElement>, originalIndex: number) => {
    if ((event.target as HTMLElement).closest("button, a, input, select, textarea")) return;
    setSelectedRowIndex((current) => current === originalIndex ? null : originalIndex);
  };

  const toolbarButton = "rounded-md border border-line bg-panel-elevated px-3 py-1.5 text-xs font-semibold text-ink-600 transition hover:bg-brand-50 hover:text-brand-600 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600 disabled:cursor-not-allowed disabled:opacity-45";

  return (
    <>
      <CollapsibleSection
        title={table.title}
        subtitle={`${table.rowCount.toLocaleString()} ${table.rowCount === 1 ? "row" : "rows"}`}
        defaultExpanded={false}
        actions={
          <>
            <button type="button" onClick={() => void copy("table", tableToTsv(table))} disabled={table.columns.length === 0} className={toolbarButton} aria-label={`Copy ${table.title} as TSV`}>
              {copyLabel("table", "Copy Table")}
            </button>
            <button type="button" onClick={() => selectedRow && void copy("row", rowToTsv(table, selectedRow))} disabled={!selectedRow} className={toolbarButton}>
              {copyLabel("row", "Copy Selected Row")}
            </button>
            <button type="button" onClick={() => setShowExportDialog(true)} disabled={table.columns.length === 0} className={toolbarButton}>Export CSV</button>
          </>
        }
      >
        {table.rows.length === 0 ? (
          <div className="px-4 py-8 text-center text-sm text-ink-500">No matching data.</div>
        ) : (
          <div className="chat-scrollbar max-h-[min(28rem,60vh)] overflow-auto overscroll-contain">
            <table className="w-full min-w-max border-collapse text-left text-sm">
              <thead className="bg-panel-elevated text-xs font-semibold uppercase tracking-[0.04em] text-ink-500">
                <tr>
                  {table.columns.map((column, columnIndex) => {
                    const sortable = isSortableColumn(column);
                    const activeSort = sort?.key === column.key ? sort.direction : null;
                    return (
                      <th
                        key={column.key}
                        scope="col"
                        aria-sort={activeSort ?? "none"}
                        className={`sticky top-0 z-20 whitespace-nowrap border-b border-line bg-panel-elevated px-4 py-3 ${columnIndex === 0 ? "left-0 z-30 shadow-[3px_0_5px_-4px_var(--text-secondary)]" : ""} ${isNumericColumn(column) ? "text-right" : "text-left"}`}
                      >
                        {sortable ? (
                          <button type="button" onClick={() => toggleSort(column.key)} className="inline-flex items-center gap-1.5 rounded-sm focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand-600">
                            <span>{column.label}</span>
                            <span aria-hidden="true" className={activeSort ? "text-brand-600" : "text-ink-500/55"}>
                              {activeSort === "ascending" ? "↑" : activeSort === "descending" ? "↓" : "↕"}
                            </span>
                          </button>
                        ) : column.label}
                      </th>
                    );
                  })}
                </tr>
              </thead>
              <tbody className="text-ink-800">
                {rows.map(({ row, originalIndex }) => {
                  const selected = selectedRowIndex === originalIndex;
                  return (
                    <tr
                      key={originalIndex}
                      data-selected={selected}
                      aria-selected={selected}
                      tabIndex={0}
                      onClick={(event) => handleRowClick(event, originalIndex)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter" || event.key === " ") {
                          event.preventDefault();
                          setSelectedRowIndex((current) => current === originalIndex ? null : originalIndex);
                        }
                      }}
                      className="analytics-row border-b border-line outline-none last:border-b-0 focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-brand-600"
                    >
                      {table.columns.map((column, columnIndex) => (
                        <td key={column.key} className={`max-w-80 bg-panel px-4 py-3 align-top transition-colors ${columnIndex === 0 ? "sticky left-0 z-10 shadow-[3px_0_5px_-4px_var(--text-secondary)]" : ""} ${isNumericColumn(column) ? "text-right tabular-nums" : "text-left"}`}>
                          {formatCell(row[column.key] ?? null, column)}
                        </td>
                      ))}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
        {table.truncated && <div className="border-t border-warning-border bg-warning px-4 py-2.5 text-xs text-warning-text">Result truncated at the read-only analytics limit.</div>}
      </CollapsibleSection>

      {showExportDialog && (
        <CsvExportDialog
          table={table}
          onCancel={() => setShowExportDialog(false)}
          onExport={(columnKeys) => {
            exportTableCsv(table, columnKeys);
            setShowExportDialog(false);
          }}
        />
      )}
    </>
  );
}
